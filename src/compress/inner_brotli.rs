//! Brotli-specific compression: parameter validation, the rayon-backed
//! parallel path, and the single-stream fallback.
//!
//! Encoding goes through [`mbrotli`]; `simd-brotli` is only kept as a dev
//! dependency because `mbrotli` ships no decoder and the round-trip tests
//! need one.

use crate::error::Error;

use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use super::InputBuffer;
use mbrotli::compressor::RetentionPolicy;
use mbrotli::compressor::parallel::{
    BatchConfig, ParallelCompressor, ParallelConfig, SegmentSize, TaskCount,
};
use mbrotli::{Compressor, EncoderConfig, Quality, Window};
use std::cell::RefCell;
use std::ops::RangeInclusive;

/// Default brotli window size (log2), matching `BROTLI_DEFAULT_WINDOW`.
pub const BROTLI_DEFAULT_WINDOW_BITS: u32 = 22;

/// Multiple of the section size an input must reach before it is split.
///
/// Two sections: one section is just the serial encoder, and the second is
/// what makes a split worth its ratio.
const BROTLI_MIN_SECTIONS: usize = 2;

const WINDOW_BITS_RANGE: RangeInclusive<u32> = 10..=24;

/// Validate a brotli window size (log2 of window size, `lgwin`).
pub fn validate_window_bits(window_bits: u32) -> Result<(), Error> {
    if !WINDOW_BITS_RANGE.contains(&window_bits) {
        return Err(Error::InvalidWindowBits(window_bits));
    }
    Ok(())
}

/// Validate a brotli section size in bytes.
///
/// Only zero is rejected. The encoder accepts segments between
/// [`SegmentSize::MIN`] and [`SegmentSize::MAX`], and a value outside that is
/// clamped rather than refused — see [`segment_size`].
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
    input: InputBuffer,
) -> Result<Vec<u8>, Error> {
    let window_bits = window_bits.unwrap_or(BROTLI_DEFAULT_WINDOW_BITS);
    validate_window_bits(window_bits)?;
    if let Some(section_size) = section_size {
        validate_section_size(section_size)?;
    }
    // Brotli sizes its hasher tables and ring buffer from the window alone, so
    // an input compressed at a window it cannot fill pays for the whole thing:
    // at quality 11 that is ~41 MB of tables for a 4 KiB chunk. Shrinking the
    // window to the first power of two past the input keeps every
    // back-reference the encoder could have made in range, so the output is
    // the same size to within a rounding error — measured across a 202-file,
    // 85 MiB batch at 42% less allocated for 0.001% more output.
    //
    // An empty input has nothing to reference, so it takes the smallest legal
    // window rather than the caller's; the range floor keeps every case inside
    // brotli's documented 10..=24, the same bounds `validate_window_bits`
    // holds callers to.
    let input_window_bits = input
        .len()
        .next_power_of_two()
        .trailing_zeros()
        .clamp(*WINDOW_BITS_RANGE.start(), *WINDOW_BITS_RANGE.end());
    let window = Window::standard(window_bits.min(input_window_bits) as u8)?;
    // Deriving the section size from the shrunken window is safe: the window
    // is at most one bit past the input length, so the sections it implies are
    // already wider than the input and the multi-section threshold below stays
    // out of reach. Only inputs long enough to keep the caller's full window
    // reach the sectioned path, and the shrink leaves their bytes untouched.
    let section_size = section_size
        .map(|section_size| section_size as usize)
        .unwrap_or(1usize << (u32::from(window.bits()) + 1));
    let segment = segment_size(section_size);
    let quality = Quality::try_from(level as u8)?;
    let config = EncoderConfig::default()
        .with_quality(quality)
        .with_window(window);

    // The threshold follows the clamped segment, so the size that decides
    // whether to split is the same one that decides how.
    if input.len() < BROTLI_MIN_SECTIONS * segment.get() {
        compress_single(config, input.as_ref())
    } else {
        compress_parallel(config, segment, input.as_ref())
    }
}

/// The caller's section size as a segment the encoder will accept.
///
/// `mbrotli` segments between [`SegmentSize::MIN`] and [`SegmentSize::MAX`]
/// (64 KiB and 16 MiB), a narrower range than the `sectionSize` option has
/// always advertised. Clamping keeps every previously valid value working and
/// lands it on the nearest size the encoder can actually honour, which is what
/// a caller asking for "as small as possible" or "as large as possible" meant
/// anyway.
fn segment_size(section_size: usize) -> SegmentSize {
    // mbrotli documents an inclusive 64 KiB..=16 MiB range but does not export
    // the bounds, so they are mirrored here (mbrotli 0.1.0).
    const MIN_SEGMENT: usize = 64 * 1024;
    const MAX_SEGMENT: usize = 16 * 1024 * 1024;
    SegmentSize::try_from(section_size.clamp(MIN_SEGMENT, MAX_SEGMENT))
        // Should the encoder's range ever narrow under those mirrored bounds,
        // its own default is still valid and is a better answer than refusing
        // to compress the file at all.
        .unwrap_or(SegmentSize::DEFAULT)
}

/// How much encoder workspace a worker may hold onto between files.
///
/// The cache only pays off when consecutive files on a worker share a shape,
/// and what a retained workspace costs depends entirely on that shape: at
/// quality 11 it is 1.1 MB at window 10 and 2.3 MB at window 14, but 62.9 MB
/// at window 22 and 188.7 MB at window 24. Retaining the expensive end would
/// mean every worker holding its high-water mark for the life of the process
/// — over a gigabyte across an 18-worker pool sitting idle between builds.
///
/// A budget inverts that neatly, because the shapes worth keeping and the
/// shapes worth paying for are the same ones. Small files are the bulk of a
/// bundle, are cheap to retain, and are where per-file setup is a real share
/// of the work; the rare big file releases its workspace as soon as it is
/// done, and rebuilding it costs nothing against the seconds it spends
/// compressing.
const COMPRESSOR_RETENTION: RetentionPolicy = RetentionPolicy::Bounded {
    max_bytes: 4 * 1024 * 1024,
};

thread_local! {
    /// The calling worker's serial encoder, reused across every file it takes.
    ///
    /// A `Compressor` exists to be reused: its second call at a given shape
    /// allocates nothing the first already paid for, and a batch is overwhelmingly
    /// files below [`parallel_threshold`] all landing here. Building one per file
    /// instead threw that away and re-paid for brotli's hasher tables and ring
    /// buffer every time.
    ///
    /// One per worker rather than one shared behind a lock: the encoder is
    /// `&mut`-driven, so sharing would serialize the batch it is meant to
    /// parallelize.
    static COMPRESSOR: RefCell<Option<Compressor>> = const { RefCell::new(None) };
}

/// Compress as one uninterrupted stream on the calling thread.
///
/// Splitting is not free under `mbrotli` the way it was under the previous
/// encoder: the fragmented stream costs about 5% of ratio against a single
/// stream on real JS (5.1% at 8 MiB, 5.8% at 16 MiB, 6.7% at 32 MiB, quality
/// 11 at the default window). Inputs below [`parallel_threshold`] are cheap
/// enough to encode serially that they should not pay it, and a cross-file
/// rayon batch keeps the other cores busy meanwhile.
///
/// The encoder is reconfigured rather than rebuilt. Quality comes from the
/// caller and the window is re-derived per input, so consecutive files rarely
/// share a shape; `reconfigure` is transactional — it drops every trace of the
/// previous stream while keeping whatever buffers still apply — so the output
/// is what a fresh `Compressor` would have produced.
fn compress_single(config: EncoderConfig, input: &[u8]) -> Result<Vec<u8>, Error> {
    COMPRESSOR.with_borrow_mut(|slot| {
        match slot {
            Some(compressor) => compressor.reconfigure(config)?,
            None => *slot = Some(Compressor::new(config)?),
        }
        let compressor = slot
            .as_mut()
            .expect("compressor is present after the match above");
        let compressed = compressor.compress(input).map_err(Error::from);
        // Hand back anything above the budget rather than holding this file's
        // workspace until the worker's next one, which may never come.
        compressor.trim(COMPRESSOR_RETENTION);
        compressed
    })
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
/// `minimum_parallel_size` is pinned wide open so that any input still small
/// enough to fit a single section takes `mbrotli`'s serial shortcut. That
/// matters: forcing fragment framing onto a single-section input costs real
/// ratio for nothing (2.8% at 4 MiB, 6.4% at 64 KiB, 11.2% at 4 KiB).
fn compress_parallel(
    config: EncoderConfig,
    segment: SegmentSize,
    input: &[u8],
) -> Result<Vec<u8>, Error> {
    let tasks = TaskCount::try_from(rayon::current_num_threads().max(1))?;

    let mut compressor = ParallelCompressor::new(config, ParallelConfig::from(segment))?;
    let mut prepared = compressor.prepare_slice(input, BatchConfig::auto(tasks))?;

    prepared
        .take_tasks()?
        .into_par_iter()
        .with_max_len(1)
        .for_each(|task| task.run());

    let mut output = Vec::new();
    prepared.finish_into(&mut output)?;

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::Algorithm;
    use crate::compress::tests::{compress as compress_any, decompress, pseudo_random};

    /// Default section size (two windows) and the split threshold derived from
    /// it, mirroring the on-the-fly computation in `compress`.
    const DEFAULT_SECTION_SIZE: usize = 1 << (BROTLI_DEFAULT_WINDOW_BITS + 1);
    const DEFAULT_THRESHOLD: usize = BROTLI_MIN_SECTIONS * DEFAULT_SECTION_SIZE;

    fn config(level: u8, window_bits: u32) -> EncoderConfig {
        EncoderConfig::default()
            .with_quality(Quality::try_from(level).expect("quality"))
            .with_window(Window::standard(window_bits as u8).expect("window"))
    }

    /// Number of tasks the encoder prepares for `input` at `section_size`.
    fn task_count(window_bits: u32, section_size: usize, input: &[u8]) -> usize {
        let tasks = TaskCount::try_from(rayon::current_num_threads().max(1)).expect("task count");
        let mut compressor = ParallelCompressor::new(
            config(5, window_bits),
            ParallelConfig::from(segment_size(section_size))
                .with_minimum_parallel_size(u64::MAX)
                .with_max_retained_workers(0),
        )
        .expect("compressor");
        let probe = BatchConfig::memory(tasks, usize::MAX);
        let estimate = compressor
            .estimate_source(input.len() as u64, &probe)
            .expect("estimate");
        let bound = estimate
            .maximum_staged_bytes
            .saturating_add(4096 * (estimate.segment_count + 1)) as usize;
        let mut prepared = compressor
            .prepare_slice(input, BatchConfig::memory(tasks, bound))
            .expect("prepare");
        let prepared_tasks: Vec<_> = prepared.take_tasks().expect("tasks").into_iter().collect();
        let count = prepared_tasks.len();
        // The batch still has to be driven to completion before it is dropped.
        prepared_tasks.into_iter().for_each(|task| task.run());
        let mut sink = Vec::new();
        prepared.finish_into(&mut sink).expect("finish");
        count
    }

    #[test]
    fn round_trips_large_brotli_inputs_via_multithreaded_path() {
        // Sized to cross DEFAULT_THRESHOLD and exercise the rayon path.
        // Moderate qualities keep the debug-build test runtime reasonable;
        // the splitting machinery is identical at every quality.
        let compressible = b"export const value = 42; // padding padding\n".repeat(400_000);
        assert!(compressible.len() >= DEFAULT_THRESHOLD);
        for level in [5, 9] {
            let compressed =
                compress_any(Algorithm::Brotli, level, None, None, compressible.clone())
                    .expect("compress");
            assert!(compressed.len() < compressible.len());
            assert_eq!(decompress(Algorithm::Brotli, &compressed), compressible);
        }

        let incompressible = pseudo_random(DEFAULT_THRESHOLD + 12_345);
        let compressed = compress_any(Algorithm::Brotli, 9, None, None, incompressible.clone())
            .expect("compress");
        assert_eq!(decompress(Algorithm::Brotli, &compressed), incompressible);
    }

    #[test]
    fn section_size_is_clamped_into_the_encoders_range() {
        // The option has always advertised a wider range than the encoder
        // accepts, so every previously valid value has to keep working by
        // landing on the nearest size the encoder can honour.
        assert_eq!(segment_size(1).get(), 64 * 1024);
        assert_eq!(segment_size(64 * 1024).get(), 64 * 1024);
        assert_eq!(segment_size(4 << 20).get(), 4 << 20);
        assert_eq!(segment_size(16 << 20).get(), 16 << 20);
        assert_eq!(segment_size(u32::MAX as usize).get(), 16 << 20);
        // The default at the default window is two windows, inside the range.
        assert_eq!(
            segment_size(DEFAULT_SECTION_SIZE).get(),
            DEFAULT_SECTION_SIZE
        );
    }

    #[test]
    fn section_size_changes_the_split_and_the_output() {
        // The knob is real: smaller sections cut the input into more of them,
        // which costs ratio. If this ever stops holding, `sectionSize` has
        // silently become inert again and the docs are lying.
        let input = b"export const value = 42; // padding padding\n".repeat(400_000);
        assert!(input.len() >= DEFAULT_THRESHOLD);

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
    fn splits_only_past_two_sections() {
        let threads = rayon::current_num_threads();
        // A ~1 MiB input is one section at the default and several at 256 KiB.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        assert!(input.len() < DEFAULT_THRESHOLD);
        assert_eq!(
            task_count(
                BROTLI_DEFAULT_WINDOW_BITS,
                DEFAULT_SECTION_SIZE,
                input.as_ref()
            ),
            1
        );
        assert!(task_count(BROTLI_DEFAULT_WINDOW_BITS, 256 * 1024, input.as_ref()) > 1);

        let big = b"export const value = 42; // padding padding\n".repeat(400_000);
        assert!(big.len() >= DEFAULT_THRESHOLD);
        let tasks = task_count(
            BROTLI_DEFAULT_WINDOW_BITS,
            DEFAULT_SECTION_SIZE,
            big.as_ref(),
        );
        assert!(tasks > 1 && tasks <= threads.max(1));
        assert_eq!(
            task_count(BROTLI_DEFAULT_WINDOW_BITS, DEFAULT_SECTION_SIZE, b"small"),
            1
        );
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
            let segment = segment_size(section_size);
            let baseline = compress_parallel(
                config(5, BROTLI_DEFAULT_WINDOW_BITS),
                segment,
                input.as_ref(),
            )
            .expect("compress");

            for tasks in [1usize, 2, 4, 8, 16] {
                let tasks = TaskCount::try_from(tasks).expect("task count");
                let mut compressor = ParallelCompressor::new(
                    config(5, BROTLI_DEFAULT_WINDOW_BITS),
                    ParallelConfig::from(segment)
                        .with_minimum_parallel_size(u64::MAX)
                        .with_max_retained_workers(0),
                )
                .expect("compressor");
                let probe = BatchConfig::memory(tasks, usize::MAX);
                let estimate = compressor
                    .estimate_source(input.len() as u64, &probe)
                    .expect("estimate");
                let bound = estimate
                    .maximum_staged_bytes
                    .saturating_add(4096 * (estimate.segment_count + 1))
                    as usize;
                let mut prepared = compressor
                    .prepare_slice(input.as_ref(), BatchConfig::memory(tasks, bound))
                    .expect("prepare");
                prepared
                    .take_tasks()
                    .expect("tasks")
                    .into_iter()
                    .for_each(|task| task.run());
                let mut compressed = Vec::new();
                prepared.finish_into(&mut compressed).expect("finish");
                assert_eq!(
                    compressed, baseline,
                    "output changed at {tasks:?} tasks, section size {section_size}"
                );
            }
            assert_eq!(decompress(Algorithm::Brotli, &baseline), input);
        }
    }

    #[test]
    fn the_encoders_own_estimate_covers_the_batch() {
        // `staging_bound` used to be a hand-fitted formula; it is now whatever
        // the encoder says it needs. If that ever stops being enough,
        // `prepare_slice` fails at runtime, so pin it with the sizes most
        // likely to expose an off-by-something.
        for len in [
            0usize,
            1,
            64 * 1024,
            DEFAULT_THRESHOLD,
            DEFAULT_THRESHOLD + 12_345,
        ] {
            let input = pseudo_random(len);
            for section_size in [64 * 1024usize, DEFAULT_SECTION_SIZE] {
                compress_parallel(
                    config(5, BROTLI_DEFAULT_WINDOW_BITS),
                    segment_size(section_size),
                    input.as_ref(),
                )
                .unwrap_or_else(|e| panic!("len {len}, section {section_size}: {e}"));
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
        assert!(input.len() >= BROTLI_MIN_SECTIONS * section_size as usize);
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
        assert!(input.len() >= DEFAULT_THRESHOLD);
        let first =
            compress_any(Algorithm::Brotli, 5, None, None, input.clone()).expect("compress");
        let second =
            compress_any(Algorithm::Brotli, 5, None, None, input.clone()).expect("compress");
        assert_eq!(first, second);
    }

    #[test]
    fn round_trips_inputs_across_the_window_shrink_boundary() {
        // The window follows the input length, so these sizes span the empty
        // input, the floor every input under 512 bytes shares, the powers of
        // two where the derived window steps, and sizes well past it. A window
        // narrower than brotli accepts would still compress here but produce a
        // stream other decoders reject, so the round-trip is the assertion.
        for len in [0usize, 1, 2, 511, 512, 513, 4096, 65_536] {
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
        // Everything else is clamped rather than refused.
        assert!(compress_any(Algorithm::Brotli, 11, None, Some(1), b"x".to_vec()).is_ok());
        assert!(compress_any(Algorithm::Brotli, 11, None, Some(u32::MAX), b"x".to_vec()).is_ok());
    }

    #[test]
    fn honors_custom_section_size() {
        // 256 KiB sections push a ~1 MB input through the multithreaded path
        // that the default section size would compress single-threaded.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let section_size = 256 * 1024u32;
        assert!(input.len() >= BROTLI_MIN_SECTIONS * section_size as usize);
        assert!(input.len() < DEFAULT_THRESHOLD);
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
        assert!(input.len() < DEFAULT_THRESHOLD);
        assert_eq!(
            task_count(window_bits, DEFAULT_SECTION_SIZE, input.as_ref()),
            1
        );
        assert!(task_count(window_bits, 1 << (window_bits + 1), input.as_ref()) > 1);

        let compressed = compress_any(Algorithm::Brotli, 5, Some(window_bits), None, input.clone())
            .expect("compress");
        assert!(compressed.len() < input.len());
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn reused_compressor_matches_a_fresh_one_across_shape_changes() {
        // The thread-local encoder is reconfigured, not rebuilt, so a batch
        // walks one `Compressor` through every quality and window it meets.
        // If any state survived a reconfigure the output would drift from
        // what a fresh encoder produces — silently, and only for whichever
        // file happened to follow a different shape.
        //
        // The shapes are deliberately interleaved so each call finds the
        // encoder dirty from a different quality *and* window, and each is
        // run twice to catch state that only shows up on reuse at the same
        // shape.
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
                let fresh = Compressor::new(cfg)
                    .expect("compressor")
                    .compress(input.as_ref())
                    .expect("compress");
                let reused = compress_single(cfg, input.as_ref()).expect("compress");
                assert_eq!(
                    reused, fresh,
                    "reused encoder drifted at quality {level}, window {window_bits}, len {len}"
                );
                assert_eq!(decompress(Algorithm::Brotli, &reused), input);
            }
        }
    }

    #[test]
    fn a_big_file_does_not_leave_its_workspace_retained() {
        // Without the budget the worker would hold this file's workspace for
        // the life of the process — 62.9 MB at quality 11 and window 22, once
        // per worker. The point of the cache is the many small files, so the
        // one big file has to hand its memory back.
        let input = b"export const value = 42; // padding padding\n".repeat(200_000);
        assert!(
            input.len() > 4 * 1024 * 1024,
            "input must need a big window"
        );
        compress_single(config(11, 22), input.as_ref()).expect("compress");
        let retained = COMPRESSOR.with_borrow(|slot| {
            slot.as_ref()
                .expect("compressor was cached")
                .retained_bytes()
        });
        assert!(
            retained <= 4 * 1024 * 1024,
            "worker retained {retained} bytes, over the {} byte budget",
            4 * 1024 * 1024
        );

        // ...and the encoder is still usable afterwards.
        let small = b"export const value = 42;\n".repeat(10);
        let reused = compress_single(config(11, 22), small.as_ref()).expect("compress");
        assert_eq!(decompress(Algorithm::Brotli, &reused), small);
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
    fn serial_and_parallel_paths_agree_on_single_section_inputs() {
        // An input that fits one section takes mbrotli's serial shortcut, so
        // the two paths have to produce the same stream; if they ever diverge,
        // the threshold becomes an observable ratio cliff.
        let input = b"export const value = 42; // padding padding\n".repeat(20_000);
        assert!(input.len() < DEFAULT_SECTION_SIZE);
        assert_eq!(
            compress_single(config(5, 22), input.as_ref()).expect("serial"),
            compress_parallel(
                config(5, 22),
                segment_size(DEFAULT_SECTION_SIZE),
                input.as_ref()
            )
            .expect("parallel"),
        );
    }
}
