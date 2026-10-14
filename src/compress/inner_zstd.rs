//! Zstd-specific compression: the rayon-backed parallel encoder and
//! per-thread encoder reuse.
//!
//! Encoding goes through [`mzstd`]; the C `zstd` crate is only kept as a dev
//! dependency, so the round-trip tests decode with the reference
//! implementation rather than with the encoder's own crate.

use crate::error::Error;

use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use mzstd::compressor::parallel::{ParallelCompressor, ParallelConfig, TaskCount};
use mzstd::{CompressionParams, EncoderConfig, Level};
use std::cell::RefCell;

/// Largest window (log2) a browser decodes: RFC 9659 caps the `zstd`
/// content coding at 8 MiB, and Chrome and Firefox reject larger frames.
pub const ZSTD_MAX_WINDOW_LOG: u32 = 23;

/// First level whose default window exceeds [`ZSTD_MAX_WINDOW_LOG`]: zstd's
/// level table gives levels 20, 21 and 22 windows of 2^25, 2^26 and 2^27.
const FIRST_OVERSIZED_WINDOW_LEVEL: u32 = 20;

/// Compress `input` with zstd at `level`.
///
/// Inputs that fit one segment (4 MiB, `mzstd`'s default) are compressed
/// serially and match the reference encoder byte for byte. Larger inputs are
/// cut into segments spread over the rayon pool: the frame is a standard
/// Zstandard frame, but not the one a single stream would write.
///
/// As with brotli, the output does not depend on the machine's core count or
/// on `concurrency`: the segment size alone fixes the segmentation, and how
/// many threads work through it leaves the bytes alone.
///
/// Levels 20-22 are capped at an 8 MiB window so browsers can decode them.
/// Only those levels get an explicit window: below them the level's own
/// window is already smaller, and setting one would widen it. Inputs up to
/// 8 MiB are unaffected, since zstd shrinks the window to the input anyway.
#[hotpath::measure(label = "compress_zstd")]
pub fn compress(level: u32, input: &[u8]) -> Result<Vec<u8>, Error> {
    let mut config = EncoderConfig::default().with_level(Level::try_from(level as i32)?);
    if level >= FIRST_OVERSIZED_WINDOW_LEVEL {
        config =
            config.with_params(CompressionParams::default().with_window_log(ZSTD_MAX_WINDOW_LOG));
    }
    compress_parallel(config, ParallelConfig::default(), input)
}

thread_local! {
    static COMPRESSOR: RefCell<Option<ParallelCompressor>> = const { RefCell::new(None) };
}

/// Compress by cutting `input` into `parallel`-sized segments and spreading
/// them over the rayon pool.
///
/// The task count is the pool width; `mzstd` caps it at the number of
/// segments the input holds, and it never changes the frame.
///
/// Each worker keeps its compressor, and with it the engines (match tables)
/// its tasks ran on, for the next file. `mzstd` cannot reconfigure a
/// compressor, so a change of configuration starts a fresh one; within a
/// batch every file shares one zstd configuration.
fn compress_parallel(
    config: EncoderConfig,
    parallel: ParallelConfig,
    input: &[u8],
) -> Result<Vec<u8>, Error> {
    let tasks = TaskCount::try_from(rayon::current_num_threads().max(1))?;

    // Rayon may run another file on this thread while joining segment tasks.
    // Release the TLS borrow before starting work so nested calls can use the cache.
    let cached = COMPRESSOR.with_borrow_mut(Option::take);
    let mut compressor = match cached {
        Some(compressor)
            if compressor.config() == &config && compressor.parallel_config() == &parallel =>
        {
            compressor
        }
        _ => ParallelCompressor::new(config, parallel)?,
    };
    let mut prepared = compressor.prepare_slice(input, tasks);
    let tasks = prepared.take_tasks();
    let split = tasks.len() > 1;
    // A single task runs inline, for the same reason as brotli's: joining a
    // parallel iterator lets rayon nest other files' jobs on this stack.
    if split {
        tasks
            .into_par_iter()
            .with_max_len(1)
            .for_each(|task| task.run());
    } else {
        tasks.into_iter().for_each(|task| task.run());
    }
    let mut output = Vec::new();
    prepared.finish_into(&mut output)?;
    // Every task of a split input returns its engine to this compressor's
    // pool, sized for the whole input. Keeping them would leave one worker
    // holding an engine per pool thread for the life of the process, so a
    // split batch keeps its configuration but gives its engines back.
    if split {
        compressor.release();
    }
    COMPRESSOR.with_borrow_mut(|slot| *slot = Some(compressor));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::Algorithm;
    use crate::compress::tests::{compress as compress_any, decompress, pseudo_random};
    use mzstd::compressor::parallel::SegmentSize;

    const SEGMENT: usize = SegmentSize::DEFAULT.get();

    fn config(level: i32) -> EncoderConfig {
        EncoderConfig::default().with_level(Level::try_from(level).expect("level"))
    }

    fn fresh_compression(level: i32, segment: usize, tasks: usize, input: &[u8]) -> Vec<u8> {
        let mut compressor = ParallelCompressor::new(
            config(level),
            ParallelConfig::default()
                .with_segment_size(SegmentSize::try_from(segment).expect("segment")),
        )
        .expect("compressor");
        compressor
            .prepare_slice(input, TaskCount::try_from(tasks).expect("tasks"))
            .finish()
            .expect("compress")
    }

    #[test]
    fn single_segments_match_the_reference_encoder() {
        // Below one segment the frame is the serial one, and the serial
        // encoder is byte-identical to zstd 1.5.7.
        for len in [0usize, 1, 1000, 200_000, SEGMENT] {
            let input: Vec<u8> = b"export const value = 42; // padding padding\n"
                .iter()
                .copied()
                .cycle()
                .take(len)
                .collect();
            for level in [1, 3, 19] {
                let output = compress(level as u32, &input).expect("compress");
                let reference = zstd::bulk::compress(&input, level).expect("reference");
                assert_eq!(output, reference, "len {len} at level {level}");
            }
        }
    }

    /// The window a frame header asks the decoder for, in bytes.
    fn header_window(frame: &[u8]) -> u64 {
        let descriptor = frame[4];
        let single_segment = descriptor & 0x20 != 0;
        if !single_segment {
            let exponent = u32::from(frame[5] >> 3) + 10;
            let mantissa = u64::from(frame[5] & 7);
            let base = 1u64 << exponent;
            return base + (base / 8) * mantissa;
        }
        // A single-segment frame's window is its content size.
        let at = 5;
        match descriptor >> 6 {
            0 => u64::from(frame[at]),
            1 => u64::from(u16::from_le_bytes([frame[at], frame[at + 1]])) + 256,
            2 => u64::from(u32::from_le_bytes(frame[at..at + 4].try_into().unwrap())),
            _ => u64::from_le_bytes(frame[at..at + 8].try_into().unwrap()),
        }
    }

    /// Decodes as a browser does, refusing windows above 8 MiB.
    fn browser_decode(frame: &[u8]) -> Vec<u8> {
        use std::io::Read;
        let mut decoder = zstd::stream::read::Decoder::new(frame).expect("decoder");
        decoder
            .window_log_max(ZSTD_MAX_WINDOW_LOG)
            .expect("window limit");
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .expect("browser-limited decode");
        out
    }

    #[test]
    fn every_level_stays_within_the_browser_window() {
        // Above 8 MiB the uncapped levels 20-22 would declare 32-128 MiB.
        // Highly repetitive input keeps the ultra levels fast in debug builds.
        let input = b"export const value = 42;\n".repeat(400_000);
        assert!(input.len() > 1 << ZSTD_MAX_WINDOW_LOG);
        for level in [1, 19, 20, 22] {
            let frame = compress(level, &input).expect("compress");
            assert!(
                header_window(&frame) <= 1 << ZSTD_MAX_WINDOW_LOG,
                "level {level} declares a {} byte window",
                header_window(&frame)
            );
            assert_eq!(browser_decode(&frame), input, "level {level}");
        }
    }

    #[test]
    fn window_cap_leaves_inputs_up_to_the_cap_unchanged() {
        // zstd shrinks the window to the input, so the cap only changes
        // frames for inputs above 8 MiB; below, levels 20-22 still match
        // the reference encoder at the plain level.
        let large = b"export const value = 42; // padding padding\n".repeat(75_000);
        assert!(large.len() > 3 << 20 && large.len() <= SEGMENT);
        for input in [
            Vec::new(),
            pseudo_random(1000),
            pseudo_random(300_000),
            large,
        ] {
            let len = input.len();
            for level in [20, 21, 22] {
                let output = compress(level as u32, &input).expect("compress");
                let reference = zstd::bulk::compress(&input, level).expect("reference");
                assert_eq!(output, reference, "len {len} at level {level}");
            }
        }
    }

    #[test]
    fn round_trips_large_zstd_inputs_via_multithreaded_path() {
        let compressible = b"export const value = 42; // padding padding\n".repeat(250_000);
        assert!(compressible.len() > 2 * SEGMENT);
        for level in [3, 19] {
            let compressed = compress_any(Algorithm::Zstd, level, None, None, compressible.clone())
                .expect("compress");
            assert!(compressed.len() < compressible.len() / 100);
            assert_eq!(decompress(Algorithm::Zstd, &compressed), compressible);
        }

        let incompressible = pseudo_random(SEGMENT + 12_345);
        let compressed =
            compress_any(Algorithm::Zstd, 3, None, None, incompressible.clone()).expect("compress");
        assert_eq!(decompress(Algorithm::Zstd, &compressed), incompressible);
    }

    #[test]
    fn splits_as_soon_as_input_exceeds_one_segment() {
        for len in [SEGMENT - 1, SEGMENT, SEGMENT + 1, 2 * SEGMENT + 1] {
            let input = pseudo_random(len);
            let output = compress(3, &input).expect("compress");
            assert_eq!(
                output,
                fresh_compression(3, SEGMENT, 2, &input),
                "len {len}"
            );
            assert_eq!(
                output == zstd::bulk::compress(&input, 3).expect("reference"),
                len <= SEGMENT,
                "len {len}"
            );
            assert_eq!(decompress(Algorithm::Zstd, &output), input);
        }
    }

    #[test]
    fn output_does_not_depend_on_the_task_count() {
        let input = b"function chunk(a, b) { return a + b; }\n".repeat(300_000);
        assert!(input.len() > 2 * SEGMENT);
        let baseline = compress(3, &input).expect("compress");
        for tasks in [1usize, 2, 3, 8, 16] {
            assert_eq!(
                fresh_compression(3, SEGMENT, tasks, &input),
                baseline,
                "output changed at {tasks} tasks"
            );
        }
        assert_eq!(decompress(Algorithm::Zstd, &baseline), input);
    }

    #[test]
    fn reused_compressor_matches_a_fresh_one_across_shape_changes() {
        // Alternate level and size, crossing the split threshold, on one thread.
        let shapes = [
            (19, 40_000usize),
            (1, 700),
            (19, SEGMENT + 1),
            (3, 0),
            (3, 2 * SEGMENT + 7),
            (19, 700),
            (22, 90_000),
        ];
        for _ in 0..2 {
            for (level, len) in shapes {
                let input: Vec<u8> = b"export const value = 42; // padding padding\n"
                    .iter()
                    .copied()
                    .cycle()
                    .take(len)
                    .collect();
                let reused = compress(level as u32, &input).expect("compress");
                assert_eq!(
                    reused,
                    fresh_compression(level, SEGMENT, 2, &input),
                    "reused encoder drifted at level {level}, len {len}"
                );
                assert_eq!(decompress(Algorithm::Zstd, &reused), input);
            }
        }
    }

    #[test]
    fn concurrent_parallel_compressions_do_not_deadlock() {
        // Every rayon worker inside a split compression at once; see the
        // brotli test of the same name.
        let input = b"export const value = 42; // padding padding\n".repeat(100_000);
        assert!(input.len() > SEGMENT);
        let jobs = 4 * rayon::current_num_threads();
        let compressed: Vec<_> = (0..jobs)
            .into_par_iter()
            .map(|_| compress_any(Algorithm::Zstd, 3, None, None, input.clone()).expect("compress"))
            .collect();
        for output in &compressed {
            assert_eq!(output, &compressed[0]);
        }
        assert_eq!(decompress(Algorithm::Zstd, &compressed[0]), input);
    }

    #[test]
    fn every_worker_gets_its_own_compressor() {
        let jobs = 32 * rayon::current_num_threads();
        let outputs: Vec<_> = (0..jobs)
            .into_par_iter()
            .map(|i| {
                // Vary the level per job so workers keep replacing compressors.
                let level = 1 + (i % 22) as u32;
                let input = b"function chunk(a, b) { return a + b; }\n".repeat(100 + i % 500);
                let compressed = compress_any(Algorithm::Zstd, level, None, None, input.clone())
                    .expect("compress");
                (level, input, compressed)
            })
            .collect();
        for (level, input, compressed) in outputs {
            assert_eq!(
                compressed,
                zstd::bulk::compress(&input, level as i32).expect("reference"),
                "worker-dependent output at level {level}"
            );
        }
    }

    #[test]
    fn split_batches_release_their_engines() {
        let input = pseudo_random(2 * SEGMENT + 1);
        compress(3, &input).expect("compress");
        let retained = COMPRESSOR.with_borrow(|slot| slot.as_ref().map(|c| c.retained_bytes()));
        assert_eq!(retained, Some(0));

        compress(3, b"small inputs keep their engine").expect("compress");
        let retained = COMPRESSOR.with_borrow(|slot| slot.as_ref().map(|c| c.retained_bytes()));
        assert!(retained.is_some_and(|bytes| bytes > 0));
    }
}
