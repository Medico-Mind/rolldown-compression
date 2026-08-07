//! Brotli-specific compression: parameter validation, the rayon-backed
//! sectioned path, and the single-stream fallback.

mod buf;
mod pool;
mod shared_input;

use super::InputBuffer;
use brotli::enc::threading::{CompressMulti, Owned, SendAlloc};
use brotli::enc::{BrotliEncoderMaxCompressedSize, BrotliEncoderParams};
use brotli::enc::{BrotliEncoderMaxCompressedSizeMulti, SliceWrapper, StandardAlloc, UnionHasher};
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Default brotli window size (log2), matching `BROTLI_DEFAULT_WINDOW`.
pub const BROTLI_DEFAULT_WINDOW_BITS: u32 = 22;

const BROTLI_MIN_SECTIONS: usize = 2;

const WINDOW_BITS_RANGE: RangeInclusive<u32> = 10..=24;

/// Validate a brotli window size (log2 of window size, `lgwin`).
pub fn validate_window_bits(window_bits: u32) -> Result<(), String> {
    if !WINDOW_BITS_RANGE.contains(&window_bits) {
        return Err(format!(
            "invalid brotli windowBits {window_bits}: expected 10..=24"
        ));
    }
    Ok(())
}

/// Validate a brotli section size in bytes.
pub fn validate_section_size(section_size: u32) -> Result<(), String> {
    if section_size == 0 {
        return Err(format!(
            "invalid brotli sectionSize {section_size}: expected a positive number of bytes"
        ));
    }
    Ok(())
}

/// Compress `input` with brotli, applying the brotli-only defaults for
/// `window_bits` and `section_size` when the caller left them unset.
///
/// The section size defaults to two full windows: sections much smaller than
/// the window lose too many cross-section matches.
pub fn compress(
    level: u32,
    window_bits: Option<u32>,
    section_size: Option<u32>,
    input: InputBuffer,
) -> Result<Vec<u8>, String> {
    let window_bits = window_bits.unwrap_or(BROTLI_DEFAULT_WINDOW_BITS);
    validate_window_bits(window_bits)?;
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
    let window_bits = window_bits.min(input_window_bits);
    // Deriving the section size from the shrunken window is safe: the window
    // is at most one bit past the input length, so the sections it implies are
    // already wider than the input and the multi-section threshold below stays
    // out of reach. Only inputs long enough to keep the caller's full window
    // reach the sectioned path, and the shrink leaves their bytes untouched.
    let section_size = section_size.unwrap_or(1 << (window_bits + 1));
    validate_section_size(section_size)?;
    compress_sectioned(level, window_bits, section_size as usize, input)
}

/// Compress large inputs by splitting them into ~`section_size` sections
/// spread over the rayon pool; smaller inputs stay in one section on the
/// calling thread.
///
/// Inputs holding at least `BROTLI_MIN_SECTIONS` full sections are split
/// (16 MiB at the default section size); below that a cross-file rayon batch
/// already keeps all cores busy and splitting would only cost ratio.
/// Sectioning costs a fraction of a percent of ratio versus a single stream,
/// in exchange for finishing the large files that dominate a batch tail
/// several times faster.
///
/// The count is one section per full `section_size`, capped at one section per
/// rayon worker: past that, sections queue behind each other in uneven waves
/// and the extra split only costs ratio. Measured on 100 MiB of JS at quality
/// 11 with 18 workers: 4 sections 19.0s, 12 sections 8.2s, 18 sections 6.1s,
/// 24 sections 8.1s, with 0.1% between the smallest and largest output.
///
/// Below the cap the count follows the input length alone, so those inputs
/// compress to the same bytes on every machine; inputs past
/// `threads * section_size` are cut into as many sections as the pool is wide,
/// so their bytes depend on the pool size — the machine's core count, or
/// `concurrency` when it is set.
#[hotpath::measure(label = "compress_brotli")]
fn compress_sectioned(
    quality: u32,
    window_bits: u32,
    section_size: usize,
    input: InputBuffer,
) -> Result<Vec<u8>, String> {
    let input_len = input.len();
    let params = BrotliEncoderParams {
        quality: quality as i32,
        lgwin: window_bits as i32,
        size_hint: input_len,
        ..Default::default()
    };
    if input_len >= BROTLI_MIN_SECTIONS * section_size {
        // `max` keeps the bound above the minimum on a one-thread pool, where
        // `clamp` would otherwise panic on an inverted range.
        let max_sections = rayon::current_num_threads().max(BROTLI_MIN_SECTIONS);
        let num_sections = (input_len / section_size).clamp(BROTLI_MIN_SECTIONS, max_sections);
        compress_multi(&params, num_sections, shared_input::SharedInput(input))
    } else {
        buf::BROTLI_BUFFER
            .with_borrow_mut(|buffer| compress_single(&params, input.as_ref(), buffer))
    }
}

#[hotpath::measure(label = "compress_brotli_single")]
fn compress_single(
    params: &BrotliEncoderParams,
    input: &[u8],
    buffer: &mut buf::BrotliBuf,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(BrotliEncoderMaxCompressedSize(input.len()));
    let mut reader = input;
    let (input_buf, output_buf) = buffer.split();
    brotli::BrotliCompressCustomAlloc(
        &mut reader,
        &mut output,
        input_buf,
        output_buf,
        params,
        StandardAlloc::default(),
    )
    .map_err(|err| format!("brotli compression failed: {err}"))?;
    Ok(output)
}

#[hotpath::measure(label = "compress_brotli_multi")]
fn compress_multi(
    params: &BrotliEncoderParams,
    num_sections: usize,
    input: shared_input::SharedInput,
) -> Result<Vec<u8>, String> {
    let input_len = input.slice().len();
    let mut output = vec![0u8; BrotliEncoderMaxCompressedSizeMulti(input_len, num_sections)];
    let mut alloc_per_section: Vec<_> = (0..num_sections)
        .map(|_| SendAlloc::new(StandardAlloc::default(), UnionHasher::Uninit))
        .collect();
    // The last section is compressed inline by `CompressMulti`, so a panic
    // there unwinds through here rather than through a spawned task.
    let written = catch_unwind(AssertUnwindSafe(|| {
        CompressMulti(
            params,
            &mut Owned::new(input),
            &mut output,
            &mut alloc_per_section,
            &mut pool::RayonBrotliWorkerPool,
        )
    }))
    .map_err(|_| "brotli compression failed: panic handled".to_string())?
    .map_err(|err| format!("brotli compression failed: {err:?}"))?;
    output.truncate(written);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::Algorithm;
    use crate::compress::tests::{compress as compress_any, decompress, pseudo_random};

    /// Default section size (two windows, `2^(window_bits + 1)` bytes) and the
    /// multi-section threshold derived from it, mirroring the on-the-fly
    /// computation in `compress`.
    const DEFAULT_SECTION_SIZE: usize = 1 << (BROTLI_DEFAULT_WINDOW_BITS + 1);
    const DEFAULT_MULTI_THRESHOLD: usize = BROTLI_MIN_SECTIONS * DEFAULT_SECTION_SIZE;

    #[test]
    fn round_trips_large_brotli_inputs_via_multithreaded_path() {
        // Sized to cross DEFAULT_MULTI_THRESHOLD and exercise the rayon path.
        // Moderate qualities keep the debug-build test runtime reasonable;
        // the sectioning machinery is identical at every quality.
        let compressible = b"export const value = 42; // padding padding\n".repeat(382_000);
        assert!(compressible.len() >= DEFAULT_MULTI_THRESHOLD);
        for level in [5, 9] {
            let compressed =
                compress_any(Algorithm::Brotli, level, None, None, compressible.clone())
                    .expect("compress");
            assert!(compressed.len() < compressible.len());
            assert_eq!(decompress(Algorithm::Brotli, &compressed), compressible);
        }

        let incompressible = pseudo_random(DEFAULT_MULTI_THRESHOLD + 12_345);
        let compressed = compress_any(Algorithm::Brotli, 9, None, None, incompressible.clone())
            .expect("compress");
        assert_eq!(decompress(Algorithm::Brotli, &compressed), incompressible);
    }

    #[test]
    fn rayon_spawner_round_trips_multi_section_inputs() {
        // Drives `compress_multi` directly with more sections than the public
        // path clamps to, so the rayon spawner is exercised with more
        // outstanding sections than there are workers to run them.
        let input = b"export const value = 42; // padding padding\n".repeat(382_000);
        let num_sections = rayon::current_num_threads() + 2;
        let params = BrotliEncoderParams {
            quality: 5,
            lgwin: BROTLI_DEFAULT_WINDOW_BITS as i32,
            size_hint: input.len(),
            ..Default::default()
        };
        let compressed = compress_multi(
            &params,
            num_sections,
            shared_input::SharedInput(input.clone()),
        )
        .expect("compress");
        assert!(compressed.len() < input.len());
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn concurrent_sectioned_compressions_do_not_deadlock() {
        // Puts every rayon worker inside a sectioned compression at once. If
        // joining a section parked its worker instead of working the queue,
        // no thread would be left to run the spawned sections and this would
        // hang rather than fail.
        use rayon::prelude::*;

        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let section_size = 256 * 1024_u32;
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
    fn multithreaded_path_engages_for_large_inputs() {
        // Sectioned output has different block boundaries than a single
        // stream, so equality with the single-threaded encoder means the
        // sectioned path silently fell back (as a lazy-init bug once did).
        let input = b"export const value = 42; // padding padding\n".repeat(382_000);
        assert!(input.len() >= DEFAULT_MULTI_THRESHOLD);
        let params = BrotliEncoderParams {
            quality: 5,
            lgwin: BROTLI_DEFAULT_WINDOW_BITS as i32,
            size_hint: input.len(),
            ..Default::default()
        };
        let single = compress_single(&params, input.as_ref(), &mut buf::BrotliBuf::default())
            .expect("compress");
        let compressed =
            compress_any(Algorithm::Brotli, 5, None, None, input.clone()).expect("compress");
        assert_ne!(
            compressed, single,
            "large input should take the sectioned rayon path, not the single-stream encoder"
        );
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn multithreaded_brotli_is_deterministic() {
        let input = b"function chunk(a, b) { return a + b; }\n".repeat(431_000);
        assert!(input.len() >= DEFAULT_MULTI_THRESHOLD);
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
        assert!(compress_any(Algorithm::Brotli, 11, None, Some(1), b"x".to_vec()).is_ok());
    }

    #[test]
    fn honors_custom_section_size() {
        // 256 KiB sections push a ~1 MB input through the multithreaded path
        // that the default section size would compress single-threaded.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let section_size = 256 * 1024_u32;
        assert!(input.len() >= BROTLI_MIN_SECTIONS * section_size as usize);
        assert!(input.len() < DEFAULT_MULTI_THRESHOLD);
        let compressed = compress_any(
            Algorithm::Brotli,
            5,
            None,
            Some(section_size),
            input.clone(),
        )
        .expect("compress");
        assert!(compressed.len() < input.len());
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn derives_default_section_size_from_window_bits() {
        // With no explicit section size the default is two windows
        // (2^(windowBits + 1) bytes), so windowBits 18 gives 512 KiB sections
        // and this ~1 MB input crosses the 2x multithreading threshold that
        // the default window would compress single-threaded. Sectioned output
        // differs from the single-stream encoder's, which proves the
        // derived default engaged the worker-pool path.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let window_bits = 18_u32;
        assert!(input.len() >= BROTLI_MIN_SECTIONS << (window_bits + 1));
        assert!(input.len() < DEFAULT_MULTI_THRESHOLD);
        let params = BrotliEncoderParams {
            quality: 5,
            lgwin: window_bits as i32,
            size_hint: input.len(),
            ..Default::default()
        };
        let single = compress_single(&params, input.as_ref(), &mut buf::BrotliBuf::default())
            .expect("compress");
        let compressed = compress_any(Algorithm::Brotli, 5, Some(window_bits), None, input.clone())
            .expect("compress");
        assert_ne!(
            compressed, single,
            "default section size should follow windowBits onto the sectioned path"
        );
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }
}
