#![deny(clippy::all)]

pub mod compress;
pub mod scheduler;

#[cfg(not(target_family = "wasm"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// The napi glue references Node-API symbols that only exist inside a Node.js
// process, so it is compiled out of the `cargo test` harness. All logic worth
// testing lives in `compress` and `scheduler`.
#[cfg(not(test))]
mod binding {
    use napi::bindgen_prelude::*;
    use napi_derive::napi;
    use rayon::prelude::*;

    use crate::compress::{Algorithm, validate_section_size, validate_window_bits};
    use crate::scheduler::{BatchItem, BatchOutcome, run_batch};

    /// One compression task: pairs with the buffer at the same index in the
    /// `buffers` argument of [`compress_buffers`].
    #[napi(object)]
    pub struct CompressTask {
        pub file_name: String,
        /// Canonical algorithm name: "gzip" | "brotli" | "zstd".
        pub algorithm: String,
        /// Compression level; algorithm default when omitted
        /// (gzip 6, brotli 11, zstd 19).
        pub level: Option<u32>,
        /// Brotli only: log2 window size (10-24, default 22).
        pub window_bits: Option<u32>,
        /// Brotli only: target section size in bytes per worker thread when
        /// large inputs are split across the brotli worker pool; inputs at
        /// least four times this size take the multithreaded path. Defaults
        /// to one window (`2^windowBits` bytes), i.e. 4 MiB and
        /// multithreading from 16 MiB at the default window.
        pub section_size: Option<u32>,
    }

    /// Batch-wide options for [`compress_buffers`].
    #[napi(object)]
    pub struct BatchOptions {
        /// Worker threads; 0 or omitted = number of logical CPUs.
        pub concurrency: Option<u32>,
        /// Mark results whose compressed size >= original as skipped and
        /// return no data for them. Default: false.
        pub skip_if_larger_or_equal: Option<bool>,
    }

    /// Outcome of a single task within a batch.
    #[napi(object)]
    pub struct CompressResult {
        pub file_name: String,
        pub algorithm: String,
        /// Compressed bytes. Empty when `skipped` is true or `error` is set.
        pub data: Buffer,
        pub original_size: u32,
        pub compressed_size: u32,
        /// True when compression would not shrink the input and
        /// `skipIfLargerOrEqual` was requested.
        pub skipped: bool,
        /// Per-task failure. A failed task never aborts the rest of the batch.
        pub error: Option<String>,
    }

    struct ParsedTask {
        file_name: String,
        algorithm: Algorithm,
        level: u32,
        window_bits: Option<u32>,
        section_size: Option<u32>,
    }

    pub struct CompressWorker {
        tasks: Vec<ParsedTask>,
        buffers: Vec<Buffer>,
        skip_if_larger_or_equal: bool,
    }

    pub struct WorkerOutcome {
        file_name: String,
        algorithm: Algorithm,
        original_size: u32,
        outcome: BatchOutcome,
    }

    #[napi]
    impl Task for CompressWorker {
        type Output = Vec<WorkerOutcome>;
        type JsValue = Vec<CompressResult>;

        fn compute(&mut self) -> Result<Self::Output> {
            let tasks = std::mem::take(&mut self.tasks);
            let buffers = std::mem::take(&mut self.buffers);
            let skip_if_larger_or_equal = self.skip_if_larger_or_equal;

            // Each buffer is moved into its item so the scheduler drops the
            // reference to the JS-side allocation as soon as that item is
            // compressed, instead of pinning every input until the batch
            // resolves on the event loop. That consumes the input, so the
            // reporting metadata is split off here.
            let (metadata, items): (Vec<_>, Vec<BatchItem>) = tasks
                .into_par_iter()
                .zip(buffers)
                .map(|(task, buffer)| {
                    let original_size = buffer.len() as u32;
                    (
                        (task.file_name, task.algorithm, original_size),
                        BatchItem {
                            algorithm: task.algorithm,
                            level: task.level,
                            window_bits: task.window_bits,
                            section_size: task.section_size,
                            input: buffer,
                        },
                    )
                })
                .unzip();

            let outcomes = run_batch(items, skip_if_larger_or_equal);

            Ok(metadata
                .into_par_iter()
                .zip(outcomes)
                .map(
                    |((file_name, algorithm, original_size), outcome)| WorkerOutcome {
                        file_name,
                        algorithm,
                        original_size,
                        outcome,
                    },
                )
                .collect())
        }

        fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
            Ok(output
                .into_par_iter()
                .map(|result| CompressResult {
                    file_name: result.file_name,
                    algorithm: result.algorithm.to_string(),
                    compressed_size: result.outcome.data.len() as u32,
                    data: result.outcome.data.into(),
                    original_size: result.original_size,
                    skipped: result.outcome.skipped,
                    error: result.outcome.error,
                })
                .collect())
        }
    }

    /// Guards the one and only `build_global` attempt of this process.
    static GLOBAL_POOL: std::sync::Once = std::sync::Once::new();

    /// Size rayon's global pool to `concurrency` threads.
    ///
    /// The global pool can only be built once per process, so the first batch
    /// that requests an explicit concurrency wins and every later request is a
    /// no-op — including one asking for a different thread count. Without an
    /// explicit concurrency the pool keeps rayon's default sizing, one thread
    /// per logical CPU.
    ///
    /// A pool that cannot be configured is not fatal: compression still runs
    /// on whatever global pool exists, so the failure is only warned about.
    fn configure_global_pool(concurrency: usize) {
        GLOBAL_POOL.call_once(|| {
            if let Err(err) = rayon::ThreadPoolBuilder::new()
                .num_threads(concurrency)
                .build_global()
            {
                eprintln!(
                    "warning: could not size the compression thread pool to {concurrency} threads ({err}); using the default pool instead"
                );
            }
        });
    }

    /// Compress a batch of buffers off the JS main thread.
    ///
    /// `tasks[i]` describes how to compress `buffers[i]`. Algorithm names and
    /// levels are validated synchronously so misconfiguration fails fast;
    /// I/O-shaped failures during compression are reported per task via
    /// [`CompressResult::error`].
    #[napi]
    pub fn compress_buffers(
        tasks: Vec<CompressTask>,
        buffers: Vec<Buffer>,
        options: Option<BatchOptions>,
    ) -> Result<AsyncTask<CompressWorker>> {
        if tasks.len() != buffers.len() {
            return Err(Error::new(
                Status::InvalidArg,
                format!(
                    "tasks and buffers must have the same length (got {} tasks, {} buffers)",
                    tasks.len(),
                    buffers.len()
                ),
            ));
        }

        for buffer in &buffers {
            if buffer.len() > u32::MAX as usize {
                return Err(Error::new(
                    Status::InvalidArg,
                    "buffers larger than 4 GiB are not supported",
                ));
            }
        }

        let mut parsed = Vec::with_capacity(tasks.len());
        for task in tasks {
            let algorithm = task
                .algorithm
                .parse::<Algorithm>()
                .map_err(|err| Error::new(Status::InvalidArg, err))?;
            let level = task.level.unwrap_or_else(|| algorithm.default_level());
            algorithm
                .validate_level(level)
                .map_err(|err| Error::new(Status::InvalidArg, err))?;
            if algorithm == Algorithm::Brotli {
                if let Some(window_bits) = task.window_bits {
                    validate_window_bits(window_bits)
                        .map_err(|err| Error::new(Status::InvalidArg, err))?;
                }
                if let Some(section_size) = task.section_size {
                    validate_section_size(section_size)
                        .map_err(|err| Error::new(Status::InvalidArg, err))?;
                }
            }
            parsed.push(ParsedTask {
                file_name: task.file_name,
                algorithm,
                level,
                window_bits: task.window_bits,
                section_size: task.section_size,
            });
        }

        let concurrency = options
            .as_ref()
            .and_then(|options| options.concurrency)
            .filter(|concurrency| *concurrency > 0);
        let skip_if_larger_or_equal = options
            .as_ref()
            .and_then(|options| options.skip_if_larger_or_equal)
            .unwrap_or(false);

        if let Some(concurrency) = concurrency {
            configure_global_pool(concurrency as usize);
        }

        Ok(AsyncTask::new(CompressWorker {
            tasks: parsed,
            buffers,
            skip_if_larger_or_equal,
        }))
    }
}
