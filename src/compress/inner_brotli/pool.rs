use rayon::Yield;
use simd_brotli::Allocator;
use simd_brotli::enc::threading::{
    InternalOwned, InternalSendAlloc, Joinable, OwnedRetriever, PoisonedThreadError,
};
use simd_brotli::enc::{
    BatchSpawnableLite, BrotliAlloc, BrotliEncoderThreadError, Owned, SendAlloc,
};
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::time::Duration;

/// How long [`RayonJoinable::join`] parks once rayon reports nothing left to
/// steal. Short enough that a section becoming stealable again is picked up
/// promptly, long enough not to spin.
const BROTLI_JOIN_POLL_INTERVAL: Duration = Duration::from_micros(250);

/// Section spawner for [`CompressMulti`] backed by the global rayon pool.
///
/// Brotli's own `WorkerPool` owns dedicated OS threads that have to be kept
/// alive between calls to be worth their spawn cost. Running sections on rayon
/// instead means they share the very threads that already run the surrounding
/// per-file batch: no extra threads, no pool lifecycle, and no oversubscription
/// when many large files are compressed at once.
#[derive(Copy, Clone)]
pub struct RayonBrotliWorkerPool;

/// The input shared with every in-flight section.
///
/// Sections only ever read it — brotli's own spawners reach for
/// `Arc<RwLock<U>>` because that is the one [`OwnedRetriever`] impl the crate
/// ships, not because anything writes — so a bare [`Arc`] carries it with no
/// lock to take and no poisoning to handle. `U: Sync` at the spawn site is what
/// makes sharing `&U` across sections sound.
pub struct SharedSections<U>(Arc<U>);

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
pub struct RayonJoinable<ReturnValue> {
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
