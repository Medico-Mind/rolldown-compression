//! Parallel batch scheduling on top of rayon.
//!
//! Unit tested with plain `cargo test`: [`InputBuffer`] switches to
//! `Vec<u8>` under test, so no Node-API symbols are referenced.

use std::panic::{AssertUnwindSafe, catch_unwind};

use rayon::prelude::*;

use crate::compress::{Algorithm, InputBuffer, compress};

/// A single unit of compression work.
///
/// Owns its input so the scheduler can release each buffer as soon as its
/// item finishes compressing instead of pinning the whole batch in memory
/// until every item is done.
pub struct BatchItem {
    pub algorithm: Algorithm,
    pub level: u32,
    pub window_bits: Option<u32>,
    pub section_size: Option<u32>,
    pub input: InputBuffer,
}

/// The outcome of one [`BatchItem`].
///
/// Exactly one of the following holds:
/// - `error` is `Some`: the task failed, `data` is empty and `skipped` is false;
/// - `skipped` is true: compressed output was >= input size and skipping was
///   requested, `data` is empty;
/// - otherwise `data` holds the compressed bytes.
#[derive(Default, Clone)]
pub struct BatchOutcome {
    pub data: Vec<u8>,
    pub skipped: bool,
    pub error: Option<String>,
}

/// Run every item of the batch in parallel and return outcomes in input order.
///
/// Work runs on the caller's ambient rayon pool: the global one unless the
/// caller wraps this in [`rayon::ThreadPool::install`] to pin the batch to a
/// dedicated pool.
///
/// * `skip_if_larger_or_equal` — mark items whose compressed size would be
///   `>=` the input size as skipped instead of returning the bloated output.
///
/// A failure (or panic) of a single item never aborts the batch; it is
/// reported through [`BatchOutcome::error`].
#[hotpath::measure]
pub fn run_batch(items: Vec<BatchItem>, skip_if_larger_or_equal: bool) -> Vec<BatchOutcome> {
    let mut outcomes: Vec<BatchOutcome> = Vec::with_capacity(items.len());

    items
        .into_par_iter()
        .map(|item| run_one(item, skip_if_larger_or_equal))
        .collect_into_vec(&mut outcomes);

    outcomes
}

#[hotpath::measure]
fn run_one(item: BatchItem, skip_if_larger_or_equal: bool) -> BatchOutcome {
    let input_len = item.input.len();
    let algorithm = item.algorithm;
    // `compress` consumes the input and drops it as soon as compression
    // finishes, releasing the buffer per item instead of holding the whole
    // batch until the last item ends.
    let result = catch_unwind(AssertUnwindSafe(|| {
        compress(
            item.algorithm,
            item.level,
            item.window_bits,
            item.section_size,
            item.input,
        )
    }))
    .unwrap_or_else(|_| Err(format!("{} compression panicked unexpectedly", algorithm)));

    match result {
        Ok(data) if skip_if_larger_or_equal && data.len() >= input_len => BatchOutcome {
            data: Vec::new(),
            skipped: true,
            error: None,
        },
        Ok(data) => BatchOutcome {
            data,
            skipped: false,
            error: None,
        },
        Err(error) => BatchOutcome {
            data: Vec::new(),
            skipped: false,
            error: Some(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_fixture(seed: usize) -> Vec<u8> {
        format!("export const value{seed} = {seed};\n")
            .repeat(200 + seed * 7)
            .into_bytes()
    }

    /// Run a batch on a dedicated pool, the way the napi binding does.
    /// `threads` of 0 means the rayon default (one per logical CPU).
    fn run_batch_on_pool(
        items: Vec<BatchItem>,
        threads: usize,
        skip_if_larger_or_equal: bool,
    ) -> Vec<BatchOutcome> {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("build pool")
            .install(|| run_batch(items, skip_if_larger_or_equal))
    }

    fn make_items(inputs: &[Vec<u8>]) -> Vec<BatchItem> {
        let algorithms = [Algorithm::Gzip, Algorithm::Brotli, Algorithm::Zstd];
        inputs
            .iter()
            .enumerate()
            .map(|(i, input)| {
                let algorithm = algorithms[i % algorithms.len()];
                BatchItem {
                    algorithm,
                    level: algorithm.default_level(),
                    window_bits: None,
                    section_size: None,
                    input: input.clone(),
                }
            })
            .collect()
    }

    #[test]
    fn batch_preserves_input_order_and_succeeds() {
        let inputs: Vec<Vec<u8>> = (0..24).map(text_fixture).collect();
        let outcomes = run_batch_on_pool(make_items(&inputs), 0, false);
        assert_eq!(outcomes.len(), inputs.len());
        for outcome in &outcomes {
            assert!(outcome.error.is_none());
            assert!(!outcome.skipped);
            assert!(!outcome.data.is_empty());
        }
    }

    #[test]
    fn batch_is_deterministic_across_thread_counts() {
        // Scheduling never affects output for inputs this size. Brotli inputs
        // past `threads * sectionSize` are the one exception — they are cut
        // into as many sections as the pool is wide — so keep the fixtures
        // well under the sectioning threshold.
        let inputs: Vec<Vec<u8>> = (0..24).map(text_fixture).collect();

        let single = run_batch_on_pool(make_items(&inputs), 1, false);
        for threads in [2, 4, 8] {
            let multi = run_batch_on_pool(make_items(&inputs), threads, false);
            assert_eq!(single.len(), multi.len());
            for (a, b) in single.iter().zip(multi.iter()) {
                assert_eq!(a.data, b.data, "output differs with {threads} threads");
            }
        }
    }

    #[test]
    fn skip_if_larger_or_equal_marks_incompressible_items() {
        // 4 bytes of data always grow under any container format.
        let input = vec![1u8, 2, 3, 4];
        let make_items = || {
            vec![BatchItem {
                algorithm: Algorithm::Gzip,
                level: 6,
                window_bits: None,
                section_size: None,
                input: input.clone(),
            }]
        };
        let outcomes = run_batch_on_pool(make_items(), 0, true);
        assert!(outcomes[0].skipped);
        assert!(outcomes[0].data.is_empty());
        assert!(outcomes[0].error.is_none());

        let outcomes = run_batch_on_pool(make_items(), 0, false);
        assert!(!outcomes[0].skipped);
        assert!(outcomes[0].data.len() > input.len());
    }

    #[test]
    fn single_failure_does_not_abort_batch() {
        let good = b"hello world hello world hello world".to_vec();
        let items = vec![
            BatchItem {
                algorithm: Algorithm::Gzip,
                level: 6,
                window_bits: None,
                section_size: None,
                input: good.clone(),
            },
            BatchItem {
                // Invalid level sneaks past FFI validation only in theory,
                // but the scheduler must still isolate the failure.
                algorithm: Algorithm::Zstd,
                level: 99,
                window_bits: None,
                section_size: None,
                input: good.clone(),
            },
        ];
        let outcomes = run_batch_on_pool(items, 0, false);
        assert!(outcomes[0].error.is_none());
        assert!(!outcomes[0].data.is_empty());
        assert!(outcomes[1].error.is_some());
        assert!(outcomes[1].data.is_empty());
    }
}
