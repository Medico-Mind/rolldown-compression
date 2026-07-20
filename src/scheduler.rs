//! Parallel batch scheduling on top of rayon.
//!
//! Pure Rust module with no napi dependency so it can be unit tested with
//! plain `cargo test`.

use std::panic::{AssertUnwindSafe, catch_unwind};

use rayon::prelude::*;

use crate::compress::{Algorithm, compress};

/// A single unit of compression work.
pub struct BatchItem<'a> {
    pub algorithm: Algorithm,
    pub level: u32,
    pub window_bits: Option<u32>,
    pub input: &'a [u8],
}

/// The outcome of one [`BatchItem`].
///
/// Exactly one of the following holds:
/// - `error` is `Some`: the task failed, `data` is empty and `skipped` is false;
/// - `skipped` is true: compressed output was >= input size and skipping was
///   requested, `data` is empty;
/// - otherwise `data` holds the compressed bytes.
#[derive(Clone)]
pub struct BatchOutcome {
    pub data: Vec<u8>,
    pub skipped: bool,
    pub error: Option<String>,
}

/// Run every item of the batch in parallel and return outcomes in input order.
///
/// * `concurrency` — number of worker threads; `0` uses the rayon default
///   (number of logical CPUs).
/// * `skip_if_larger_or_equal` — mark items whose compressed size would be
///   `>=` the input size as skipped instead of returning the bloated output.
///
/// A failure (or panic) of a single item never aborts the batch; it is
/// reported through [`BatchOutcome::error`].
pub fn run_batch(
    items: &[BatchItem<'_>],
    concurrency: usize,
    skip_if_larger_or_equal: bool,
) -> Vec<BatchOutcome> {
    // Most-expensive-first scheduling: a large brotli task started late would
    // stretch the batch tail on an otherwise idle pool. Outcomes are written
    // back by index, so output order (and determinism) is unaffected.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&index| std::cmp::Reverse(estimated_cost(&items[index])));

    let work = || {
        let mut outcomes: Vec<Option<BatchOutcome>> = vec![Default::default(); items.len()];
        let computed: Vec<(usize, BatchOutcome)> = order
            .par_iter()
            // Allow every item to be stolen individually; batches of a few
            // large files balance poorly with rayon's default chunking.
            .with_max_len(1)
            .map(|&index| (index, run_one(&items[index], skip_if_larger_or_equal)))
            .collect();
        for (index, outcome) in computed {
            outcomes[index] = Some(outcome);
        }
        outcomes.into_iter().map(Option::unwrap).collect()
    };

    if concurrency == 0 {
        return work();
    }

    match rayon::ThreadPoolBuilder::new()
        .num_threads(concurrency)
        .build()
    {
        Ok(pool) => pool.install(work),
        // Falling back to the global pool is better than failing the batch.
        Err(_) => work(),
    }
}

/// Rough relative CPU cost of an item, used only for scheduling order.
/// Derived from measured per-byte throughput of each algorithm/level class.
fn estimated_cost(item: &BatchItem<'_>) -> u64 {
    let weight = match item.algorithm {
        Algorithm::Gzip => 1 + u64::from(item.level) / 3,
        Algorithm::Brotli => match item.level {
            10.. => 60,
            7..=9 => 12,
            _ => 4,
        },
        Algorithm::Zstd => match item.level {
            18.. => 16,
            10..=17 => 6,
            _ => 2,
        },
    };
    item.input.len() as u64 * weight
}

fn run_one(item: &BatchItem<'_>, skip_if_larger_or_equal: bool) -> BatchOutcome {
    let result = catch_unwind(AssertUnwindSafe(|| {
        compress(item.algorithm, item.level, item.window_bits, item.input)
    }))
    .unwrap_or_else(|_| {
        Err(format!(
            "{} compression panicked unexpectedly",
            item.algorithm.name()
        ))
    });

    match result {
        Ok(data) => {
            if skip_if_larger_or_equal && data.len() >= item.input.len() {
                BatchOutcome {
                    data: Vec::new(),
                    skipped: true,
                    error: None,
                }
            } else {
                BatchOutcome {
                    data,
                    skipped: false,
                    error: None,
                }
            }
        }
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

    fn make_items(inputs: &[Vec<u8>]) -> Vec<BatchItem<'_>> {
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
                    input,
                }
            })
            .collect()
    }

    #[test]
    fn batch_preserves_input_order_and_succeeds() {
        let inputs: Vec<Vec<u8>> = (0..24).map(text_fixture).collect();
        let items = make_items(&inputs);
        let outcomes = run_batch(&items, 0, false);
        assert_eq!(outcomes.len(), items.len());
        for outcome in &outcomes {
            assert!(outcome.error.is_none());
            assert!(!outcome.skipped);
            assert!(!outcome.data.is_empty());
        }
    }

    #[test]
    fn batch_is_deterministic_across_thread_counts() {
        let inputs: Vec<Vec<u8>> = (0..24).map(text_fixture).collect();
        let items = make_items(&inputs);

        let single = run_batch(&items, 1, false);
        for threads in [2, 4, 8] {
            let multi = run_batch(&items, threads, false);
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
        let items = vec![BatchItem {
            algorithm: Algorithm::Gzip,
            level: 6,
            window_bits: None,
            input: &input,
        }];
        let outcomes = run_batch(&items, 0, true);
        assert!(outcomes[0].skipped);
        assert!(outcomes[0].data.is_empty());
        assert!(outcomes[0].error.is_none());

        let outcomes = run_batch(&items, 0, false);
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
                input: &good,
            },
            BatchItem {
                // Invalid level sneaks past FFI validation only in theory,
                // but the scheduler must still isolate the failure.
                algorithm: Algorithm::Zstd,
                level: 99,
                window_bits: None,
                input: &good,
            },
        ];
        let outcomes = run_batch(&items, 0, false);
        assert!(outcomes[0].error.is_none());
        assert!(!outcomes[0].data.is_empty());
        assert!(outcomes[1].error.is_some());
        assert!(outcomes[1].data.is_empty());
    }
}
