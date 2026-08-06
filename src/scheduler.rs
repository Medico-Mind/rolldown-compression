//! Parallel batch scheduling on top of rayon.
//!
//! Unit tested with plain `cargo test`: [`InputBuffer`] switches to
//! `Vec<u8>` under test, so no Node-API symbols are referenced.

use std::cmp::Reverse;
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

/// Scheduling rank of an algorithm: brotli runs first, zstd next, gzip last.
///
/// Rayon hands items to workers roughly in iteration order, so putting the
/// slowest algorithm at the front lets its long tail overlap with the cheap
/// work instead of trailing behind it.
fn algorithm_rank(algorithm: Algorithm) -> u8 {
    match algorithm {
        Algorithm::Brotli => 0,
        Algorithm::Zstd => 1,
        Algorithm::Gzip => 2,
    }
}

/// Run every item of the batch in parallel and return outcomes in input order.
///
/// Items are scheduled longest-job-first — brotli, then zstd, then gzip, and
/// the largest input first inside each algorithm — and the outcomes are put
/// back into input order before returning.
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
pub fn run_batch(mut items: Vec<BatchItem>, skip_if_larger_or_equal: bool) -> Vec<BatchOutcome> {
    // `order[scheduled position] == input position`. Four bytes per item is
    // the whole cost of the reordering: both permutations below run in place,
    // so neither the items nor the outcomes are ever copied into a second
    // buffer. A batch can never hold more than `u32::MAX` items — each one
    // owns a live input buffer.
    let mut order: Vec<u32> = (0..items.len() as u32).collect();
    order.sort_unstable_by_key(|&i| {
        let item = &items[i as usize];
        (algorithm_rank(item.algorithm), Reverse(item.input.len()))
    });
    gather_in_place(&mut items, &order);

    let mut outcomes: Vec<BatchOutcome> = Vec::with_capacity(items.len());

    items
        .into_par_iter()
        .with_max_len(1)
        .map(|item| run_one(item, skip_if_larger_or_equal))
        .collect_into_vec(&mut outcomes);

    // Outcomes come back in scheduled order; `order` says where each belongs.
    scatter_in_place(&mut outcomes, &mut order);

    outcomes
}

/// Rearrange `data` so that `data[i]` holds what used to be at `order[i]`.
///
/// `order` must be a permutation of `0..data.len()`; it is left untouched, and
/// elements only ever move by swapping — nothing is cloned or buffered.
fn gather_in_place<T>(data: &mut [T], order: &[u32]) {
    for target in 0..data.len() {
        // Slots below `target` are already final: whatever they held has been
        // swapped further along, so follow the chain to where it sits now.
        let mut source = order[target] as usize;
        while source < target {
            source = order[source] as usize;
        }
        data.swap(target, source);
    }
}

/// Move every `data[i]` to index `order[i]`, the inverse of
/// [`gather_in_place`]. `order` is used as scratch and left as the identity
/// permutation.
fn scatter_in_place<T>(data: &mut [T], order: &mut [u32]) {
    for i in 0..data.len() {
        // Each swap parks at least one element at its final index, so the
        // inner loop runs at most `len` times in total.
        while order[i] as usize != i {
            let j = order[i] as usize;
            data.swap(i, j);
            order.swap(i, j);
        }
    }
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

    /// The `rank`-th permutation of `0..n` in Lehmer-code order.
    fn permutation(n: usize, mut rank: usize) -> Vec<u32> {
        let mut pool: Vec<u32> = (0..n as u32).collect();
        (1..=n)
            .rev()
            .map(|remaining| {
                let pick = rank % remaining;
                rank /= remaining;
                pool.remove(pick)
            })
            .collect()
    }

    #[test]
    fn gather_and_scatter_invert_each_other() {
        for n in 1..=6usize {
            let factorial: usize = (1..=n).product();
            for rank in 0..factorial {
                let order = permutation(n, rank);
                let source: Vec<u32> = (0..n as u32).map(|i| i * 10).collect();

                let mut data = source.clone();
                gather_in_place(&mut data, &order);
                for (target, &from) in order.iter().enumerate() {
                    assert_eq!(
                        data[target], source[from as usize],
                        "gather n={n} rank={rank} order={order:?}"
                    );
                }

                let mut scratch = order.clone();
                scatter_in_place(&mut data, &mut scratch);
                assert_eq!(data, source, "scatter n={n} rank={rank} order={order:?}");
                assert_eq!(scratch, (0..n as u32).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn schedules_brotli_then_zstd_then_gzip_largest_first() {
        // Sizes are distinct per item so the schedule is fully determined.
        let algorithms = [
            Algorithm::Gzip,
            Algorithm::Brotli,
            Algorithm::Zstd,
            Algorithm::Gzip,
            Algorithm::Brotli,
            Algorithm::Zstd,
        ];
        let mut items: Vec<BatchItem> = algorithms
            .iter()
            .enumerate()
            .map(|(i, &algorithm)| BatchItem {
                algorithm,
                level: algorithm.default_level(),
                window_bits: None,
                section_size: None,
                input: vec![0u8; (i + 1) * 10],
            })
            .collect();

        let mut order: Vec<u32> = (0..items.len() as u32).collect();
        order.sort_unstable_by_key(|&i| {
            let item = &items[i as usize];
            (algorithm_rank(item.algorithm), Reverse(item.input.len()))
        });
        gather_in_place(&mut items, &order);

        let scheduled: Vec<(Algorithm, usize)> = items
            .iter()
            .map(|item| (item.algorithm, item.input.len()))
            .collect();
        assert_eq!(
            scheduled,
            vec![
                (Algorithm::Brotli, 50),
                (Algorithm::Brotli, 20),
                (Algorithm::Zstd, 60),
                (Algorithm::Zstd, 30),
                (Algorithm::Gzip, 40),
                (Algorithm::Gzip, 10),
            ]
        );
    }

    #[test]
    fn outcomes_stay_paired_with_their_own_input() {
        // Fixtures differ in size and algorithm, so the schedule reorders them
        // heavily; every outcome must still decompress back to its own input.
        let inputs: Vec<Vec<u8>> = (0..24).map(text_fixture).collect();
        let items = make_items(&inputs);
        let algorithms: Vec<Algorithm> = items.iter().map(|item| item.algorithm).collect();

        let outcomes = run_batch_on_pool(items, 4, false);
        assert_eq!(outcomes.len(), inputs.len());
        for ((outcome, input), algorithm) in outcomes.iter().zip(&inputs).zip(algorithms) {
            assert!(outcome.error.is_none());
            assert_eq!(&decompress(algorithm, &outcome.data), input);
        }
    }

    fn decompress(algorithm: Algorithm, input: &[u8]) -> Vec<u8> {
        use std::io::Read;
        match algorithm {
            Algorithm::Gzip => {
                let mut out = Vec::new();
                flate2::read::GzDecoder::new(input)
                    .read_to_end(&mut out)
                    .expect("gzip decode");
                out
            }
            Algorithm::Brotli => {
                let mut out = Vec::new();
                brotli::BrotliDecompress(&mut { input }, &mut out).expect("brotli decode");
                out
            }
            Algorithm::Zstd => zstd::stream::decode_all(input).expect("zstd decode"),
        }
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
