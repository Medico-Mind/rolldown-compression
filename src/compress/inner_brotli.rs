//! Brotli-specific compression: parameter validation, the shared worker-pool
//! pool backing the sectioned path, and the single-stream fallback.

use brotli::enc::threading::{Owned, SendAlloc};
use brotli::enc::{BrotliEncoderMaxCompressedSize, BrotliEncoderParams};
use brotli::enc::{
    BrotliEncoderMaxCompressedSizeMulti, CompressionThreadResult, SliceWrapper, StandardAlloc,
    UnionHasher, WorkerPool, compress_worker_pool, new_work_pool,
};
use crossbeam_deque::{Injector, Steal};
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;

use super::InputBuffer;

/// Default brotli window size (log2), matching `BROTLI_DEFAULT_WINDOW`.
pub const BROTLI_DEFAULT_WINDOW_BITS: u32 = 22;

const BROTLI_MIN_THREADS: usize = 2;
const BROTLI_MAX_THREADS: usize = 4;
const BROTLI_BUFFER_SIZE: usize = 4096;

/// Validate a brotli window size (log2 of window size, `lgwin`).
pub fn validate_window_bits(window_bits: u32) -> Result<(), String> {
    if !(10..=24).contains(&window_bits) {
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

/// Owned input for `compress_worker_pool`, which shares the buffer across
/// worker threads and therefore cannot borrow it. Newtype because the orphan
/// rule forbids implementing brotli's `SliceWrapper` for [`InputBuffer`]
/// directly.
struct SharedInput(InputBuffer);

impl SliceWrapper<u8> for SharedInput {
    fn slice(&self) -> &[u8] {
        self.0.as_ref()
    }
}

type BrotliWorkerPool = WorkerPool<
    CompressionThreadResult<StandardAlloc>,
    UnionHasher<StandardAlloc>,
    StandardAlloc,
    (SharedInput, BrotliEncoderParams),
>;

/// Cache of idle brotli worker pools, shared by every rayon worker that hits
/// the sectioned path. Spinning a pool up costs OS thread spawns, so pools are
/// checked out for the duration of one compression and returned afterwards
/// rather than rebuilt per input.
///
/// Backed by a lock-free [`Injector`]: checkout/return happen on every large
/// input from all rayon workers at once, and the queue is unordered by nature
/// (any idle pool will do), so there is nothing for a mutex to protect.
#[derive(Default)]
struct BrotliWorkerPoolPool {
    queue: Injector<BrotliWorkerPool>,
}

impl BrotliWorkerPoolPool {
    fn with_mut<T>(
        &self,
        callback: impl FnOnce(&mut BrotliWorkerPool) -> T,
    ) -> std::thread::Result<T> {
        let mut pool = self.pop();
        let res = catch_unwind(AssertUnwindSafe(|| callback(&mut pool)));
        self.queue.push(pool);
        res
    }
    #[hotpath::measure(label = "brotli_worker_pool_checkout")]
    fn pop(&self) -> BrotliWorkerPool {
        loop {
            match self.queue.steal() {
                Steal::Success(worker_pool) => return worker_pool,
                // A concurrent push/steal raced us; the queue may still hold a
                // pool, so retry rather than pay for a fresh one.
                Steal::Retry => std::thread::yield_now(),
                Steal::Empty => break,
            }
        }
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(BROTLI_MIN_THREADS, BROTLI_MAX_THREADS);
        new_work_pool(threads.saturating_sub(1))
    }
}

static BROTLI_WORKER_POOL_POOL: LazyLock<BrotliWorkerPoolPool> =
    LazyLock::new(BrotliWorkerPoolPool::default);

/// Compress `input` with brotli, applying the brotli-only defaults for
/// `window_bits` and `section_size` when the caller left them unset.
///
/// The section size defaults to one full window: sections much smaller than
/// the window lose too many cross-section matches.
pub fn compress(
    level: u32,
    window_bits: Option<u32>,
    section_size: Option<u32>,
    input: InputBuffer,
) -> Result<Vec<u8>, String> {
    let window_bits = window_bits.unwrap_or(BROTLI_DEFAULT_WINDOW_BITS);
    validate_window_bits(window_bits)?;
    let section_size = section_size.unwrap_or(1 << window_bits);
    validate_section_size(section_size)?;
    compress_sectioned(level, window_bits, section_size as usize, input)
}

/// Compress large inputs by splitting them into ~`section_size` sections
/// spread over the per-thread worker pool; smaller inputs stay in one
/// section on the calling thread.
///
/// Inputs at least four times `section_size` are split (16 MiB at the
/// default section size); below that a cross-file rayon batch already keeps
/// all cores busy and splitting would only cost ratio. The section count depends on the pool's thread budget, so output
/// bytes for inputs past that threshold are stable within a process but may
/// differ across machines or `concurrency` settings. Sectioning costs a
/// fraction of a percent of ratio versus a single stream, in exchange for
/// finishing the large files that dominate a batch tail several times faster.
///
/// On Windows every input is compressed single-threaded regardless of size:
/// the sectioned path needs a persistent `thread_local` worker pool, and
/// dropping one there deadlocks — TLS destructors run under the Windows
/// loader lock, and `WorkerPool::drop` joins worker threads that cannot exit
/// without that same lock (rust-lang/rust#74875).
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
    if input_len >= 4 * section_size {
        let num_sections = (input_len / section_size).clamp(BROTLI_MIN_THREADS, BROTLI_MAX_THREADS);
        // Check a pool out for the duration of this compression and hand it
        // back afterwards; its threads outlive the call and get reused by
        // whichever rayon worker claims the pool next.
        BROTLI_WORKER_POOL_POOL
            .with_mut(|pool| compress_pooled(&params, num_sections, pool, SharedInput(input)))
            .map_err(|_| "brotli compression failed: panic handled".to_string())?
    } else {
        compress_single(&params, input.as_ref())
    }
}

thread_local! {
    /// Scratch space for `BrotliCompressCustomAlloc`, split into an input and an
    /// output half. Heap-backed on purpose: this crate is a `cdylib` loaded with
    /// `dlopen`, and an inline `[u8; 8192]` blows past glibc's static TLS surplus
    /// ("cannot allocate memory in static TLS block") once mimalloc has taken its
    /// share, so the binding fails to load at all on linux-gnu.
    static BROTLI_BUFFER: RefCell<Vec<u8>> = RefCell::new(vec![0; BROTLI_BUFFER_SIZE * 2]);
}

#[hotpath::measure(label = "compress_brotli_single")]
fn compress_single(params: &BrotliEncoderParams, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(BrotliEncoderMaxCompressedSize(input.len()));
    let mut reader = input;
    BROTLI_BUFFER
        .with_borrow_mut(|buffer| {
            let (input_buf, output_buf) = buffer.split_at_mut(BROTLI_BUFFER_SIZE);
            brotli::BrotliCompressCustomAlloc(
                &mut reader,
                &mut output,
                input_buf,
                output_buf,
                params,
                StandardAlloc::default(),
            )
        })
        .map_err(|err| format!("brotli compression failed: {err}"))?;
    Ok(output)
}

#[hotpath::measure(label = "compress_brotli_pooled")]
fn compress_pooled(
    params: &BrotliEncoderParams,
    num_sections: usize,
    pool: &mut BrotliWorkerPool,
    input: SharedInput,
) -> Result<Vec<u8>, String> {
    let input_len = input.slice().len();
    let mut output = vec![0u8; BrotliEncoderMaxCompressedSizeMulti(input_len, num_sections)];
    let mut alloc_per_thread: Vec<_> = (0..num_sections)
        .map(|_| SendAlloc::new(StandardAlloc::default(), UnionHasher::Uninit))
        .collect();
    let written = compress_worker_pool(
        params,
        &mut Owned::new(input),
        &mut output,
        &mut alloc_per_thread,
        pool,
    )
    .map_err(|err| format!("brotli compression failed: {err:?}"))?;
    output.truncate(written);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::tests::{decompress, pseudo_random};
    use crate::compress::{Algorithm, compress as compress_any};

    /// Default section size (one window, `2^window_bits` bytes) and the
    /// multi-section threshold derived from it, mirroring the on-the-fly
    /// computation in `compress`.
    const DEFAULT_SECTION_SIZE: usize = 1 << BROTLI_DEFAULT_WINDOW_BITS;
    const DEFAULT_MULTI_THRESHOLD: usize = 4 * DEFAULT_SECTION_SIZE;

    #[test]
    fn round_trips_large_brotli_inputs_via_multithreaded_path() {
        // Sized to cross DEFAULT_MULTI_THRESHOLD and exercise the worker pool.
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
    fn worker_pool_round_trips_multi_section_inputs() {
        // The global pool's section budget depends on which test initializes
        // it first, so pin a dedicated pool to guarantee multi-section
        // coverage: 4 sections need 3 pool workers plus the calling thread.
        let input = b"export const value = 42; // padding padding\n".repeat(382_000);
        let num_sections = input.len() / DEFAULT_SECTION_SIZE;
        assert!(num_sections >= 4);
        let params = BrotliEncoderParams {
            quality: 5,
            lgwin: BROTLI_DEFAULT_WINDOW_BITS as i32,
            size_hint: input.len(),
            ..Default::default()
        };
        let mut pool = new_work_pool(num_sections - 1);
        let compressed =
            compress_pooled(&params, num_sections, &mut pool, SharedInput(input.clone()))
                .expect("compress");
        assert!(compressed.len() < input.len());
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn multithreaded_path_engages_for_large_inputs() {
        // Sectioned output has different block boundaries than a single
        // stream, so equality with the single-threaded encoder means the
        // worker-pool path silently fell back (as a lazy-init bug once did).
        let input = b"export const value = 42; // padding padding\n".repeat(382_000);
        assert!(input.len() >= DEFAULT_MULTI_THRESHOLD);
        let params = BrotliEncoderParams {
            quality: 5,
            lgwin: BROTLI_DEFAULT_WINDOW_BITS as i32,
            size_hint: input.len(),
            ..Default::default()
        };
        let single = compress_single(&params, input.as_ref()).expect("compress");
        let compressed =
            compress_any(Algorithm::Brotli, 5, None, None, input.clone()).expect("compress");
        assert_ne!(
            compressed, single,
            "large input should take the sectioned worker-pool path, not the single-stream encoder"
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
        assert!(input.len() >= 4 * section_size as usize);
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
        // With no explicit section size the default is one window
        // (2^windowBits bytes), so windowBits 18 gives 256 KiB sections and
        // this ~1 MB input crosses the 4x multithreading threshold that the
        // default window would compress single-threaded. Sectioned output
        // differs from the single-stream encoder's, which proves the
        // derived default engaged the worker-pool path.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let window_bits = 18_u32;
        assert!(input.len() >= 4 << window_bits);
        assert!(input.len() < DEFAULT_MULTI_THRESHOLD);
        let params = BrotliEncoderParams {
            quality: 5,
            lgwin: window_bits as i32,
            size_hint: input.len(),
            ..Default::default()
        };
        let single = compress_single(&params, input.as_ref()).expect("compress");
        let compressed = compress_any(Algorithm::Brotli, 5, Some(window_bits), None, input.clone())
            .expect("compress");
        assert_ne!(
            compressed, single,
            "default section size should follow windowBits onto the sectioned path"
        );
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }
}
