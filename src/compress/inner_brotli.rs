//! Brotli-specific compression: parameter validation, the rayon-backed
//! parallel encoder, and per-thread encoder reuse.
//!
//! Encoding goes through [`mbrotli`]; `simd-brotli` is only kept as a dev
//! dependency because `mbrotli` ships no decoder and the round-trip tests
//! need one.

use crate::error::Error;

use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use mbrotli::compressor::parallel::{
    BatchConfig, ParallelCompressor, ParallelConfig, SegmentSize, TaskCount,
};
use mbrotli::{EncoderConfig, Quality, Window};
use std::cell::RefCell;

/// Default brotli window size (log2), matching `BROTLI_DEFAULT_WINDOW`.
pub const BROTLI_DEFAULT_WINDOW_BITS: u32 = 22;

/// Validate the public window option before shrinking it to the input.
pub fn validate_window_bits(window_bits: u32) -> Result<(), Error> {
    if !(10..=24).contains(&window_bits) {
        return Err(Error::InvalidWindowBits(window_bits));
    }
    Ok(())
}

/// Positive section sizes are clamped to the encoder's supported range.
pub fn validate_section_size(section_size: u32) -> Result<(), Error> {
    if section_size == 0 {
        return Err(Error::InvalidSectionSize(section_size));
    }
    Ok(())
}

/// Compress `input` with brotli, applying the brotli-only defaults for
/// `window_bits` and `section_size` when the caller left them unset.
///
/// The section size defaults to two full windows: sections much smaller than
/// the window lose too many cross-section matches.
///
/// Brotli output does not depend on the machine's core count or on
/// `concurrency`. How many sections an input is cut into is fixed by
/// `section_size`, and how many threads chew through them is a scheduling
/// decision that leaves the bytes alone, so a given input and set of options
/// compress the same everywhere, as gzip and zstd already did.
pub fn compress(
    level: u32,
    window_bits: Option<u32>,
    section_size: Option<u32>,
    input: &[u8],
) -> Result<Vec<u8>, Error> {
    let window_bits = window_bits.unwrap_or(BROTLI_DEFAULT_WINDOW_BITS);

    validate_window_bits(window_bits)?;
    if let Some(section_size) = section_size {
        validate_section_size(section_size)?;
    }
    let input_window_bits = input.len().next_power_of_two().trailing_zeros().max(10);
    let window = Window::standard(window_bits.min(input_window_bits) as u8)?;

    let section_size = section_size
        .map(|section_size| section_size as usize)
        .unwrap_or(1usize << (u32::from(window.bits()) + 1));
    // mbrotli accepts 64 KiB..=16 MiB; small windows derive smaller sections.
    let segment = SegmentSize::try_from(section_size.clamp(64 << 10, 16 << 20))?;
    let quality = Quality::try_from(level as u8)?;
    let config = EncoderConfig::default()
        .with_quality(quality)
        .with_window(window);

    compress_parallel(config, segment, input)
}

thread_local! {
    static COMPRESSOR: RefCell<Option<ParallelCompressor>> = const { RefCell::new(None) };
}

/// Compress by cutting `input` into `segment`-sized sections and spreading
/// them over the rayon pool.
///
/// The task count is simply the pool width. `mbrotli` caps it at the number of
/// sections the input actually holds, so asking for more tasks than there is
/// work costs nothing, and the task count is a pure scheduling choice — the
/// segmentation, and therefore the output, is fixed by `segment` alone.
/// Measured on real JS at quality 11 with 18 workers: 3.2x at 8 MiB, 6.2x at
/// 16 MiB, 10.9x at 32 MiB against a single stream.
///
/// Single-section inputs below the encoder's default parallel threshold use
/// its serial shortcut; all other inputs use fragment encoding.
fn compress_parallel(
    config: EncoderConfig,
    segment: SegmentSize,
    input: &[u8],
) -> Result<Vec<u8>, Error> {
    let tasks = TaskCount::try_from(rayon::current_num_threads().max(1))?;

    // Rayon may run another file on this thread while joining section tasks.
    // Release the TLS borrow before starting work so nested calls can use the cache.
    let cached = COMPRESSOR.with_borrow_mut(Option::take);
    let parallel_config = ParallelConfig::from(segment);
    let mut compressor = match cached {
        Some(mut compressor) => {
            compressor.reconfigure(config)?;
            compressor.reconfigure_parallel(parallel_config);
            compressor
        }
        None => ParallelCompressor::new(config, parallel_config)?,
    };
    let mut prepared = compressor.prepare_slice(input, BatchConfig::auto(tasks))?;
    prepared
        .take_tasks()?
        .into_par_iter()
        .with_max_len(1)
        .for_each(|task| task.run());
    let mut output = Vec::new();
    prepared.finish_into(&mut output)?;
    COMPRESSOR.with_borrow_mut(|slot| *slot = Some(compressor));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::Algorithm;
    use crate::compress::tests::{compress as compress_any, decompress, pseudo_random};

    const DEFAULT_SECTION_SIZE: usize = 1 << (BROTLI_DEFAULT_WINDOW_BITS + 1);

    fn config(level: u8, window_bits: u32) -> EncoderConfig {
        EncoderConfig::default()
            .with_quality(Quality::try_from(level).expect("quality"))
            .with_window(Window::standard(window_bits as u8).expect("window"))
    }

    fn fresh_compression(
        config: EncoderConfig,
        section_size: usize,
        tasks: usize,
        input: &[u8],
    ) -> Vec<u8> {
        let mut compressor = ParallelCompressor::new(
            config,
            ParallelConfig::from(SegmentSize::try_from(section_size).expect("section size")),
        )
        .expect("compressor");
        let mut prepared = compressor
            .prepare_slice(
                input,
                BatchConfig::auto(TaskCount::try_from(tasks).expect("tasks")),
            )
            .expect("prepare");
        prepared.run_inline().expect("run");
        let mut output = Vec::new();
        prepared.finish_into(&mut output).expect("finish");
        output
    }

    #[test]
    fn round_trips_large_brotli_inputs_via_multithreaded_path() {
        // More than one default section exercises the rayon path.
        // Moderate qualities keep the debug-build test runtime reasonable;
        // the splitting machinery is identical at every quality.
        let compressible = b"export const value = 42; // padding padding\n".repeat(400_000);
        assert!(compressible.len() >= DEFAULT_SECTION_SIZE);
        for level in [5, 9] {
            let compressed =
                compress_any(Algorithm::Brotli, level, None, None, compressible.clone())
                    .expect("compress");
            assert!(compressed.len() < compressible.len());
            assert_eq!(decompress(Algorithm::Brotli, &compressed), compressible);
        }

        let incompressible = pseudo_random(DEFAULT_SECTION_SIZE + 12_345);
        let compressed = compress_any(Algorithm::Brotli, 9, None, None, incompressible.clone())
            .expect("compress");
        assert_eq!(decompress(Algorithm::Brotli, &compressed), incompressible);
    }

    #[test]
    fn section_size_changes_the_split_and_the_output() {
        // The knob is real: smaller sections cut the input into more of them,
        // which costs ratio. If this ever stops holding, `sectionSize` has
        // silently become inert again and the docs are lying.
        let input = b"export const value = 42; // padding padding\n".repeat(400_000);
        assert!(input.len() >= DEFAULT_SECTION_SIZE);

        let mut outputs = Vec::new();
        for section_size in [256 * 1024u32, 1 << 20, 4 << 20] {
            let compressed = compress_any(
                Algorithm::Brotli,
                9,
                None,
                Some(section_size),
                input.clone(),
            )
            .expect("compress");
            assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
            outputs.push((section_size, compressed.len(), compressed));
        }

        for pair in outputs.windows(2) {
            let (small, small_len, small_bytes) = &pair[0];
            let (large, large_len, large_bytes) = &pair[1];
            assert_ne!(
                small_bytes, large_bytes,
                "sectionSize {small} and {large} produced identical bytes"
            );
            assert!(
                small_len >= large_len,
                "sectionSize {small} ({small_len} bytes) should not beat {large} ({large_len} bytes)"
            );
        }
    }

    #[test]
    fn splits_as_soon_as_input_exceeds_one_section() {
        const SECTION: usize = 64 * 1024;
        for len in [
            SECTION - 1,
            SECTION,
            SECTION + 1,
            2 * SECTION - 1,
            2 * SECTION,
        ] {
            let input = pseudo_random(len);
            let output = compress(5, Some(16), Some(SECTION as u32), &input).expect("compress");
            let expected = fresh_compression(config(5, 16), SECTION, 2, &input);
            assert_eq!(output, expected, "len {len}");
            assert_eq!(decompress(Algorithm::Brotli, &output), input);
        }
    }

    #[test]
    fn output_does_not_depend_on_the_task_count() {
        // The section size fixes the segmentation; the task count only decides
        // how many threads chew through it. That is what makes brotli output
        // reproducible across machines of different widths, so it is worth a
        // test rather than a comment — and it has to hold at every section
        // size, not just the default.
        let input = b"export const value = 42; // padding padding\n".repeat(400_000);
        for section_size in [256 * 1024usize, 1 << 20, DEFAULT_SECTION_SIZE] {
            let segment = SegmentSize::try_from(section_size).expect("section size");
            let baseline = compress_parallel(
                config(5, BROTLI_DEFAULT_WINDOW_BITS),
                segment,
                input.as_ref(),
            )
            .expect("compress");

            for tasks in [1usize, 2, 4, 8, 16] {
                let compressed = fresh_compression(
                    config(5, BROTLI_DEFAULT_WINDOW_BITS),
                    section_size,
                    tasks,
                    &input,
                );
                assert_eq!(
                    compressed, baseline,
                    "output changed at {tasks} tasks, section size {section_size}"
                );
            }
            assert_eq!(decompress(Algorithm::Brotli, &baseline), input);
        }
    }

    #[test]
    fn automatic_staging_round_trips_boundary_sizes() {
        for len in [
            0usize,
            1,
            64 * 1024,
            DEFAULT_SECTION_SIZE,
            DEFAULT_SECTION_SIZE + 12_345,
        ] {
            let input = pseudo_random(len);
            for section_size in [64 * 1024usize, DEFAULT_SECTION_SIZE] {
                let output = compress_parallel(
                    config(5, BROTLI_DEFAULT_WINDOW_BITS),
                    SegmentSize::try_from(section_size).expect("section size"),
                    input.as_ref(),
                )
                .unwrap_or_else(|e| panic!("len {len}, section {section_size}: {e}"));
                assert_eq!(decompress(Algorithm::Brotli, &output), input);
            }
        }
    }

    #[test]
    fn concurrent_parallel_compressions_do_not_deadlock() {
        // Puts every rayon worker inside a split compression at once. If
        // joining a task parked its worker instead of working the queue,
        // no thread would be left to run the spawned tasks and this would
        // hang rather than fail.
        use rayon::prelude::*;

        let input = b"export const value = 42; // padding padding\n".repeat(30_000);
        let section_size = 256 * 1024u32;
        assert!(input.len() > section_size as usize);
        let jobs = 8 * rayon::current_num_threads();
        let compressed: Vec<_> = (0..jobs)
            .into_par_iter()
            .map(|_| {
                compress_any(
                    Algorithm::Brotli,
                    5,
                    None,
                    Some(section_size),
                    input.clone(),
                )
                .expect("compress")
            })
            .collect();
        for output in compressed {
            assert_eq!(decompress(Algorithm::Brotli, &output), input);
        }
    }

    #[test]
    fn brotli_output_is_deterministic() {
        let input = b"function chunk(a, b) { return a + b; }\n".repeat(460_000);
        assert!(input.len() >= DEFAULT_SECTION_SIZE);
        let first =
            compress_any(Algorithm::Brotli, 5, None, None, input.clone()).expect("compress");
        let second =
            compress_any(Algorithm::Brotli, 5, None, None, input.clone()).expect("compress");
        assert_eq!(first, second);
    }

    #[test]
    fn round_trips_inputs_across_the_window_shrink_boundary() {
        // Cover the window floor and the derived section-size floor independently.
        for len in [
            0usize, 1, 2, 511, 512, 513, 1024, 1025, 4096, 16_384, 16_385, 32_768, 65_536,
        ] {
            let input: Vec<u8> = b"export const value = 42;\n"
                .iter()
                .copied()
                .cycle()
                .take(len)
                .collect();
            for level in [5, 11] {
                let compressed = compress_any(Algorithm::Brotli, level, None, None, input.clone())
                    .expect("compress");
                assert_eq!(
                    decompress(Algorithm::Brotli, &compressed),
                    input,
                    "len {len} at quality {level}"
                );
            }
        }
    }

    #[test]
    fn rejects_invalid_window_bits() {
        assert!(compress_any(Algorithm::Brotli, 11, Some(9), None, b"x".to_vec()).is_err());
        assert!(compress_any(Algorithm::Brotli, 11, Some(25), None, b"x".to_vec()).is_err());
        assert!(compress_any(Algorithm::Brotli, 11, Some(10), None, b"x".to_vec()).is_ok());
        assert!(compress_any(Algorithm::Brotli, 11, Some(24), None, b"x".to_vec()).is_ok());
    }

    #[test]
    fn rejects_invalid_section_size() {
        assert!(compress_any(Algorithm::Brotli, 11, None, Some(0), b"x".to_vec()).is_err());
    }

    #[test]
    fn section_size_is_clamped_into_the_encoders_range() {
        let input = pseudo_random(128 * 1024);
        for (requested, effective) in [
            (1, 64 * 1024),
            (64 * 1024 - 1, 64 * 1024),
            (64 * 1024, 64 * 1024),
            (16 << 20, 16 << 20),
            (u32::MAX, 16 << 20),
        ] {
            let output = compress(5, None, Some(requested), &input).expect("compress");
            let expected = fresh_compression(config(5, 17), effective, 2, &input);
            assert_eq!(output, expected, "section size {requested}");
            assert_eq!(decompress(Algorithm::Brotli, &output), input);
        }
    }

    #[test]
    fn honors_custom_section_size() {
        // 256 KiB sections push a ~1 MB input through the multithreaded path
        // that the default section size would compress single-threaded.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let section_size = 256 * 1024u32;
        assert!(input.len() > section_size as usize);
        assert!(input.len() < DEFAULT_SECTION_SIZE);
        let single =
            compress_any(Algorithm::Brotli, 5, None, None, input.clone()).expect("compress");
        let split = compress_any(
            Algorithm::Brotli,
            5,
            None,
            Some(section_size),
            input.clone(),
        )
        .expect("compress");
        assert_ne!(
            split, single,
            "a smaller section size should move this input onto the split path"
        );
        assert!(split.len() < input.len());
        assert_eq!(decompress(Algorithm::Brotli, &split), input);
    }

    #[test]
    fn derives_default_section_size_from_window_bits() {
        // With no explicit section size the default is two windows, so
        // windowBits 18 gives 512 KiB sections and this ~1 MB input splits
        // where the default window would have run it as one stream.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let window_bits = 18u32;
        assert!(input.len() < DEFAULT_SECTION_SIZE);
        let compressed = compress_any(Algorithm::Brotli, 5, Some(window_bits), None, input.clone())
            .expect("compress");
        let expected = compress(5, Some(window_bits), Some(1 << (window_bits + 1)), &input)
            .expect("explicit section size");
        assert_eq!(compressed, expected);
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn reused_compressor_matches_a_fresh_one_across_shape_changes() {
        // Alternate quality, window and segmentation on the same calling thread.
        let shapes = [
            (11u8, 22u32, 40_000usize),
            (5, 10, 700),
            (11, 10, 700),
            (0, 24, 90_000),
            (5, 22, 40_000),
            (11, 22, 40_000),
            (9, 16, 5_000),
            (0, 10, 1),
            (11, 24, 90_000),
            (5, 10, 0),
        ];
        for _ in 0..2 {
            for (level, window_bits, len) in shapes {
                let input: Vec<u8> = b"export const value = 42; // padding padding\n"
                    .iter()
                    .copied()
                    .cycle()
                    .take(len)
                    .collect();
                let cfg = config(level, window_bits);
                let section_size = if level == 11 { 64 * 1024 } else { 256 * 1024 };
                let fresh = fresh_compression(cfg, section_size, 2, &input);
                let reused = compress_parallel(
                    cfg,
                    SegmentSize::try_from(section_size).expect("section size"),
                    &input,
                )
                .expect("compress");
                assert_eq!(
                    reused, fresh,
                    "reused encoder drifted at quality {level}, window {window_bits}, len {len}"
                );
                assert_eq!(decompress(Algorithm::Brotli, &reused), input);
            }
        }
    }

    #[test]
    fn every_worker_gets_its_own_compressor() {
        // The cache is thread-local and the batch is a rayon fan-out, so the
        // same encoder must not be reached from two workers at once, and a
        // worker that steals a serial file while another compression is in
        // flight must not find the RefCell already borrowed.
        use rayon::prelude::*;

        let jobs = 32 * rayon::current_num_threads();
        let outputs: Vec<_> = (0..jobs)
            .into_par_iter()
            .map(|i| {
                // Vary the shape per job so workers keep reconfiguring.
                let level = (i % 12) as u32;
                let window_bits = 10 + (i % 15) as u32;
                let input = b"function chunk(a, b) { return a + b; }\n".repeat(100 + i % 500);
                let compressed = compress_any(
                    Algorithm::Brotli,
                    level,
                    Some(window_bits),
                    None,
                    input.clone(),
                )
                .expect("compress");
                assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
                (level, window_bits, input, compressed)
            })
            .collect();

        // Same shape and input must give the same bytes whichever worker ran it.
        for (level, window_bits, input, compressed) in outputs {
            let expected = compress_any(
                Algorithm::Brotli,
                level,
                Some(window_bits),
                None,
                input.clone(),
            )
            .expect("compress");
            assert_eq!(
                compressed, expected,
                "worker-dependent output at quality {level}, window {window_bits}"
            );
        }
    }

    #[test]
    fn unified_encoder_matches_serial_encoding_for_small_single_sections() {
        // An input that fits one section takes mbrotli's serial shortcut, so
        // the two paths have to produce the same stream; if they ever diverge,
        // the threshold becomes an observable ratio cliff.
        let input = b"export const value = 42; // padding padding\n".repeat(20_000);
        assert!(input.len() < DEFAULT_SECTION_SIZE);
        assert_eq!(
            mbrotli::Compressor::new(config(5, 22))
                .expect("compressor")
                .compress(&input)
                .expect("serial"),
            compress_parallel(
                config(5, 22),
                SegmentSize::try_from(DEFAULT_SECTION_SIZE).expect("section size"),
                input.as_ref()
            )
            .expect("parallel"),
        );
    }
}
