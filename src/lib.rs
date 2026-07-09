#![deny(clippy::all)]

pub mod compress;
pub mod scheduler;

// The napi glue references Node-API symbols that only exist inside a Node.js
// process, so it is compiled out of the `cargo test` harness. All logic worth
// testing lives in `compress` and `scheduler`.
#[cfg(not(test))]
mod binding {
    use napi::bindgen_prelude::*;
    use napi_derive::napi;

    use crate::compress::{Algorithm, validate_window_bits};
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
    }

    pub struct CompressWorker {
        tasks: Vec<ParsedTask>,
        buffers: Vec<Buffer>,
        concurrency: usize,
        skip_if_larger_or_equal: bool,
    }

    pub struct WorkerOutcome {
        file_name: String,
        algorithm: &'static str,
        original_size: u32,
        outcome: BatchOutcome,
    }

    #[napi]
    impl Task for CompressWorker {
        type Output = Vec<WorkerOutcome>;
        type JsValue = Vec<CompressResult>;

        fn compute(&mut self) -> Result<Self::Output> {
            let items: Vec<BatchItem<'_>> = self
                .tasks
                .iter()
                .zip(self.buffers.iter())
                .map(|(task, buffer)| BatchItem {
                    algorithm: task.algorithm,
                    level: task.level,
                    window_bits: task.window_bits,
                    input: buffer.as_ref(),
                })
                .collect();

            let outcomes = run_batch(&items, self.concurrency, self.skip_if_larger_or_equal);

            Ok(self
                .tasks
                .iter()
                .zip(self.buffers.iter())
                .zip(outcomes)
                .map(|((task, buffer), outcome)| WorkerOutcome {
                    file_name: task.file_name.clone(),
                    algorithm: task.algorithm.name(),
                    original_size: buffer.len() as u32,
                    outcome,
                })
                .collect())
        }

        fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
            Ok(output
                .into_iter()
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
            let algorithm = Algorithm::parse(&task.algorithm)
                .map_err(|err| Error::new(Status::InvalidArg, err))?;
            let level = task.level.unwrap_or_else(|| algorithm.default_level());
            algorithm
                .validate_level(level)
                .map_err(|err| Error::new(Status::InvalidArg, err))?;
            if algorithm == Algorithm::Brotli
                && let Some(window_bits) = task.window_bits
            {
                validate_window_bits(window_bits)
                    .map_err(|err| Error::new(Status::InvalidArg, err))?;
            }
            parsed.push(ParsedTask {
                file_name: task.file_name,
                algorithm,
                level,
                window_bits: task.window_bits,
            });
        }

        let concurrency = options
            .as_ref()
            .and_then(|options| options.concurrency)
            .unwrap_or(0) as usize;
        let skip_if_larger_or_equal = options
            .as_ref()
            .and_then(|options| options.skip_if_larger_or_equal)
            .unwrap_or(false);

        Ok(AsyncTask::new(CompressWorker {
            tasks: parsed,
            buffers,
            concurrency,
            skip_if_larger_or_equal,
        }))
    }
}
