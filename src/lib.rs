#![deny(clippy::all)]

pub mod compress;
pub mod error;
pub mod scheduler;

#[cfg(all(not(target_family = "wasm"), not(feature = "hotpath-alloc")))]
#[global_allocator]
static GLOBAL: mimalloc3::MiMalloc = mimalloc3::MiMalloc;

// `hotpath-alloc` counts allocations at the global allocator, so it has to sit
// in front of mimalloc rather than replace it: registered the other way round
// every allocation bypasses the counter and the report reads 0 B throughout.
#[cfg(all(not(target_family = "wasm"), feature = "hotpath-alloc"))]
#[global_allocator]
static GLOBAL: hotpath::CountingAllocator<mimalloc3::MiMalloc> =
    hotpath::CountingAllocator::with(mimalloc3::MiMalloc);

// The napi glue references Node-API symbols that only exist inside a Node.js
// process, so it is compiled out of the `cargo test` harness. All logic worth
// testing lives in `compress` and `scheduler`.
#[cfg(not(test))]
mod binding {
    use napi::bindgen_prelude::*;
    use napi_derive::napi;
    use rayon::prelude::*;
    use std::sync::Arc;

    use crate::compress::{Algorithm, validate_section_size, validate_window_bits};
    use crate::error::Error as CompressionError;
    use crate::scheduler::{BatchItem, BatchOutcome, run_batch};

    /// One source file, passed once regardless of the number of algorithms.
    #[napi(object)]
    pub struct CompressFile {
        pub file_name: String,
        pub data: Buffer,
    }

    /// Algorithm settings applied to every file in a batch.
    #[napi(object)]
    pub struct CompressAlgorithm {
        /// Canonical algorithm name: "gzip" | "brotli" | "zstd".
        pub algorithm: String,
        /// Compression level; algorithm default when omitted
        /// (gzip 6, brotli 11, zstd 19).
        pub level: Option<u32>,
        /// Brotli only: log2 window size (10-24, default 22).
        pub window_bits: Option<u32>,
        /// Brotli only: target section size in bytes when a large input is
        /// split across the brotli worker pool; inputs larger than one section
        /// are split. Defaults to two windows (`2^(windowBits + 1)` bytes),
        /// i.e. 8 MiB at the default window.
        ///
        /// Smaller sections finish large files faster and cost compression
        /// ratio. The encoder segments between 64 KiB and 16 MiB, and values
        /// outside that range are clamped to it.
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

    struct ParsedAlgorithm {
        algorithm: Algorithm,
        level: u32,
        window_bits: Option<u32>,
        section_size: Option<u32>,
    }

    pub struct CompressWorker {
        files: Vec<CompressFile>,
        algorithms: Vec<ParsedAlgorithm>,
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
            let files = std::mem::take(&mut self.files);
            let algorithms = &self.algorithms;
            let skip_if_larger_or_equal = self.skip_if_larger_or_equal;

            // Expand file × algorithm on a worker thread. Only Arc handles
            // are cloned: every task reads the same source allocation, which
            // is released when the last algorithm for that file finishes.
            let (metadata, items): (Vec<_>, Vec<BatchItem>) =
                hotpath::measure_block!("CompressWorker::split_tasks", {
                    files
                        .into_par_iter()
                        .flat_map_iter(|file| {
                            let original_size = file.data.len() as u32;
                            let input = Arc::new(file.data);
                            algorithms.iter().map(move |config| {
                                (
                                    (file.file_name.clone(), config.algorithm, original_size),
                                    BatchItem {
                                        algorithm: config.algorithm,
                                        level: config.level,
                                        window_bits: config.window_bits,
                                        section_size: config.section_size,
                                        input: Arc::clone(&input),
                                    },
                                )
                            })
                        })
                        .unzip()
                });

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
            // Handing the compressed bytes back to JS allocates a Node buffer
            // per result, so it is worth watching next to the compression
            // itself.
            Ok(hotpath::measure_block!("CompressWorker::resolve", {
                output
                    .into_par_iter()
                    .map(|result| CompressResult {
                        file_name: result.file_name,
                        algorithm: result.algorithm.to_string(),
                        compressed_size: result.outcome.data.len() as u32,
                        data: result.outcome.data.into(),
                        original_size: result.original_size,
                        skipped: result.outcome.skipped,
                        error: result.outcome.error.map(|error| error.to_string()),
                    })
                    .collect()
            }))
        }
    }

    /// Start the profiler on first use and print its report when the Node
    /// environment tears down.
    ///
    /// A `cdylib` has no `main` for `#[hotpath::main]` to wrap, so the guard
    /// is handed to the Node environment instead: dropping it from a cleanup
    /// hook renders the report as the process exits.
    #[cfg(feature = "hotpath")]
    fn ensure_profiling(env: &Env) {
        static PROFILER: std::sync::Once = std::sync::Once::new();
        PROFILER.call_once(|| {
            let guard = hotpath::HotpathGuardBuilder::new("compress_buffers").build();
            let _ = env.add_env_cleanup_hook(guard, drop);
        });
    }

    /// No-op: profiling is compiled out unless the `hotpath` feature is on.
    #[cfg(not(feature = "hotpath"))]
    fn ensure_profiling(_env: &Env) {}

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
    /// Each file carries one buffer; algorithm settings are shared by all files.
    /// Results follow file order, then algorithm order within each file. Empty
    /// files or algorithms arrays produce no results. Algorithm names and
    /// levels are validated synchronously so misconfiguration fails fast;
    /// I/O-shaped failures during compression are reported per task via
    /// [`CompressResult::error`].
    // `env` is supplied by napi and does not appear in the JS signature.
    #[napi]
    pub fn compress_buffers(
        env: Env,
        files: Vec<CompressFile>,
        algorithms: Vec<CompressAlgorithm>,
        options: Option<BatchOptions>,
    ) -> Result<AsyncTask<CompressWorker>> {
        ensure_profiling(&env);

        if files
            .len()
            .checked_mul(algorithms.len())
            .is_none_or(|count| count > u32::MAX as usize)
        {
            return Err(CompressionError::BatchTooLarge.into());
        }

        for file in &files {
            if file.data.len() > u32::MAX as usize {
                return Err(CompressionError::BufferTooLarge.into());
            }
        }

        let mut parsed = Vec::with_capacity(algorithms.len());
        for config in algorithms {
            let algorithm = config.algorithm.parse::<Algorithm>()?;
            let level = config.level.unwrap_or_else(|| algorithm.default_level());
            algorithm.validate_level(level)?;
            if algorithm == Algorithm::Brotli {
                if let Some(window_bits) = config.window_bits {
                    validate_window_bits(window_bits)?;
                }
                if let Some(section_size) = config.section_size {
                    validate_section_size(section_size)?;
                }
            }
            parsed.push(ParsedAlgorithm {
                algorithm,
                level,
                window_bits: config.window_bits,
                section_size: config.section_size,
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
            files,
            algorithms: parsed,
            skip_if_larger_or_equal,
        }))
    }
}
