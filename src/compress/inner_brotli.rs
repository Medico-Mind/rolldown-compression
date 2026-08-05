//! Brotli-specific compression: parameter validation, the rayon-backed
//! sectioned path, and the single-stream fallback.

use super::InputBuffer;
use brotli::Allocator;
use brotli::enc::threading::{
    BrotliEncoderThreadError, CompressMulti, InternalOwned, InternalSendAlloc, Joinable, Owned,
    OwnedRetriever, PoisonedThreadError, SendAlloc,
};
use brotli::enc::{
    BatchSpawnableLite, BrotliAlloc, BrotliEncoderMaxCompressedSize, BrotliEncoderParams,
};
use brotli::enc::{BrotliEncoderMaxCompressedSizeMulti, SliceWrapper, StandardAlloc, UnionHasher};
use rayon::Yield;
use std::cell::RefCell;
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::time::Duration;

/// Default brotli window size (log2), matching `BROTLI_DEFAULT_WINDOW`.
pub const BROTLI_DEFAULT_WINDOW_BITS: u32 = 22;

const BROTLI_MIN_SECTIONS: usize = 2;
const BROTLI_MAX_SECTIONS: usize = 4;
const BROTLI_BUFFER_SIZE: usize = 4096;

/// How long [`RayonJoinable::join`] parks once rayon reports nothing left to
/// steal. Short enough that a section becoming stealable again is picked up
/// promptly, long enough not to spin.
const BROTLI_JOIN_POLL_INTERVAL: Duration = Duration::from_micros(250);

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

/// Owned input for [`CompressMulti`], which shares the buffer across section
/// tasks and therefore cannot borrow it. Newtype because the orphan rule
/// forbids implementing brotli's `SliceWrapper` for [`InputBuffer`] directly.
struct SharedInput(InputBuffer);

impl SliceWrapper<u8> for SharedInput {
    fn slice(&self) -> &[u8] {
        self.0.as_ref()
    }
}

/// Section spawner for [`CompressMulti`] backed by the global rayon pool.
///
/// Brotli's own `WorkerPool` owns dedicated OS threads that have to be kept
/// alive between calls to be worth their spawn cost. Running sections on rayon
/// instead means they share the very threads that already run the surrounding
/// per-file batch: no extra threads, no pool lifecycle, and no oversubscription
/// when many large files are compressed at once.
#[derive(Copy, Clone)]
struct RayonBrotliWorkerPool;

/// The input shared with every in-flight section.
///
/// Sections only ever read it — brotli's own spawners reach for
/// `Arc<RwLock<U>>` because that is the one [`OwnedRetriever`] impl the crate
/// ships, not because anything writes — so a bare [`Arc`] carries it with no
/// lock to take and no poisoning to handle. `U: Sync` at the spawn site is what
/// makes sharing `&U` across sections sound.
struct SharedSections<U>(Arc<U>);

impl<U: Send + 'static> OwnedRetriever<U> for SharedSections<U> {
    fn view<T, F: FnOnce(&U) -> T>(&self, f: F) -> Result<T, PoisonedThreadError> {
        Ok(f(&self.0))
    }

    /// `CompressMulti` calls this to take the input back once every section is
    /// joined; it only succeeds if no section still holds a clone.
    fn unwrap(self) -> Result<U, PoisonedThreadError> {
        Arc::try_unwrap(self.0).map_err(|_| PoisonedThreadError::default())
    }
}

/// Handle to one section handed to rayon; its result arrives over a one-shot
/// channel.
struct RayonJoinable<ReturnValue> {
    result: Receiver<ReturnValue>,
}

impl<ReturnValue: Send + 'static> Joinable<ReturnValue, BrotliEncoderThreadError>
    for RayonJoinable<ReturnValue>
{
    fn join(self) -> Result<ReturnValue, BrotliEncoderThreadError> {
        loop {
            match self.result.try_recv() {
                Ok(result) => return Ok(result),
                // The task dropped its sender without sending: the input lock
                // was poisoned, or the section itself panicked.
                Err(TryRecvError::Disconnected) => {
                    return Err(BrotliEncoderThreadError::OtherThreadPanic);
                }
                Err(TryRecvError::Empty) => {}
            }
            // `CompressMulti` compresses the last section inline and only then
            // joins the rest, so this runs on a rayon worker. Blocking it
            // outright would take a thread away from the very sections being
            // waited on — and with every worker inside a large-file compression
            // at once, that deadlocks. Work off the queue instead of parking.
            match rayon::yield_now() {
                Some(Yield::Executed) => continue,
                // Nothing stealable: the outstanding sections are already
                // running elsewhere, so park briefly rather than spin, then
                // offer to help again in case new work showed up.
                Some(Yield::Idle) => match self.result.recv_timeout(BROTLI_JOIN_POLL_INTERVAL) {
                    Ok(result) => return Ok(result),
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(BrotliEncoderThreadError::OtherThreadPanic);
                    }
                },
                // Not a rayon worker (a single-file call, or a test): there is
                // no queue to help with, so a plain block is both correct and
                // cheapest.
                None => {
                    return self
                        .result
                        .recv()
                        .map_err(|_| BrotliEncoderThreadError::OtherThreadPanic);
                }
            }
        }
    }
}

impl<
    ReturnValue: Send + 'static,
    ExtraInput: Send + 'static,
    Alloc: BrotliAlloc + Send + 'static,
    U: Send + 'static + Sync,
> BatchSpawnableLite<ReturnValue, ExtraInput, Alloc, U> for RayonBrotliWorkerPool
where
    <Alloc as Allocator<u8>>::AllocatedMemory: Send + 'static,
{
    type JoinHandle = RayonJoinable<ReturnValue>;
    type FinalJoinHandle = SharedSections<U>;

    fn make_spawner(&mut self, input: &mut Owned<U>) -> Self::FinalJoinHandle {
        SharedSections(Arc::new(
            mem::replace(input, Owned(InternalOwned::Borrowed)).unwrap(),
        ))
    }

    fn spawn(
        &mut self,
        shared_input: &mut Self::FinalJoinHandle,
        work: &mut SendAlloc<ReturnValue, ExtraInput, Alloc, Self::JoinHandle>,
        index: usize,
        num_threads: usize,
        f: fn(ExtraInput, usize, usize, &U, Alloc) -> ReturnValue,
    ) {
        let (alloc, extra_input) = work.replace_with_default();
        let (sender, result) = sync_channel(1);
        let input = shared_input.0.clone();
        rayon::spawn_fifo(move || {
            // A panic escaping here would reach rayon's default panic handler
            // and abort the process. Dropping the sender instead turns it into
            // an ordinary `Err` from `compress`.
            let section = catch_unwind(AssertUnwindSafe(|| {
                f(extra_input, index, num_threads, &input, alloc)
            }));
            // Release our share of the input *before* publishing the result:
            // the joining thread reclaims it with `Arc::try_unwrap` as soon as
            // the last section is joined, and that fails while this clone is
            // still alive. Brotli's own worker pool orders these the same way.
            drop(input);
            if let Ok(section) = section {
                // A gone receiver means `CompressMulti` bailed out before
                // joining this section, which is normal on its error paths.
                let _ = sender.send(section);
            }
        });
        *work = SendAlloc(InternalSendAlloc::Join(RayonJoinable { result }));
    }
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
/// already keeps all cores busy and splitting would only cost ratio. The section count is
/// derived from the input length alone, so output bytes are reproducible
/// across machines and `concurrency` settings. Sectioning costs a fraction of
/// a percent of ratio versus a single stream, in exchange for finishing the
/// large files that dominate a batch tail several times faster.
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
        let num_sections =
            (input_len / section_size).clamp(BROTLI_MIN_SECTIONS, BROTLI_MAX_SECTIONS);
        compress_multi(&params, num_sections, SharedInput(input))
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

#[hotpath::measure(label = "compress_brotli_multi")]
fn compress_multi(
    params: &BrotliEncoderParams,
    num_sections: usize,
    input: SharedInput,
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
            &mut RayonBrotliWorkerPool,
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
    use crate::compress::tests::{decompress, pseudo_random};
    use crate::compress::{Algorithm, compress as compress_any};

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
        // path clamps to, so the rayon spawner is exercised with several
        // outstanding sections at once.
        let input = b"export const value = 42; // padding padding\n".repeat(382_000);
        let num_sections = BROTLI_MAX_SECTIONS + 2;
        let params = BrotliEncoderParams {
            quality: 5,
            lgwin: BROTLI_DEFAULT_WINDOW_BITS as i32,
            size_hint: input.len(),
            ..Default::default()
        };
        let compressed =
            compress_multi(&params, num_sections, SharedInput(input.clone())).expect("compress");
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
        let single = compress_single(&params, input.as_ref()).expect("compress");
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
