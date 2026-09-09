//! Parallel batch scheduling on top of rayon.
//!
//! Unit tested with plain `cargo test`: [`InputBuffer`] switches to
//! `Vec<u8>` under test, so no Node-API symbols are referenced.

use std::cmp::Reverse;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use rayon::prelude::*;

use crate::compress::{Algorithm, Compressors, InputBuffer};
use crate::error::Error;

/// Algorithm settings shared by every file in a batch.
#[derive(Clone, Copy)]
pub struct BatchAlgorithm {
    pub algorithm: Algorithm,
    pub level: u32,
    pub window_bits: Option<u32>,
    pub section_size: Option<u32>,
}

struct BatchFile {
    result_index: u32,
    input: Arc<InputBuffer>,
}

struct BatchGroup<'a> {
    config: &'a BatchAlgorithm,
    files: Vec<BatchFile>,
}

/// The outcome of compressing one file with one algorithm configuration.
///
/// Exactly one of the following holds:
/// - `error` is `Some`: the task failed, `data` is empty and `skipped` is false;
/// - `skipped` is true: compressed output was >= input size and skipping was
///   requested, `data` is empty;
/// - otherwise `data` holds the compressed bytes.
#[derive(Default)]
pub struct BatchOutcome {
    pub data: Vec<u8>,
    pub skipped: bool,
    pub error: Option<Error>,
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

/// Sort algorithms and files once, then build the groups directly.
/// Each task owns a source handle, so the last task for a file releases it.
#[hotpath::measure]
fn prepare_groups<'a>(
    inputs: Vec<Arc<InputBuffer>>,
    algorithms: &'a [BatchAlgorithm],
) -> Vec<BatchGroup<'a>> {
    let mut files: Vec<_> = inputs.into_iter().enumerate().collect();
    files.sort_unstable_by_key(|(index, input)| (Reverse(input.len()), *index));
    let mut configs: Vec<_> = algorithms.iter().enumerate().collect();
    configs.sort_unstable_by_key(|(index, config)| (algorithm_rank(config.algorithm), *index));

    configs
        .into_iter()
        .map(|(algorithm_index, config)| BatchGroup {
            config,
            files: files
                .iter()
                .map(|(file_index, input)| BatchFile {
                    result_index: (file_index * algorithms.len() + algorithm_index) as u32,
                    input: Arc::clone(input),
                })
                .collect(),
        })
        .collect()
}

/// Compress every file with every algorithm on the caller's ambient rayon pool.
///
/// Schedule brotli, then zstd, then gzip, with files largest first inside each
/// configuration. Return outcomes in file order, then configuration order.
/// The binding validates that the file × algorithm count fits u32.
///
/// `skip_if_larger_or_equal` discards compressed output that would be at least
/// as large as its input. A failure or panic is reported per task and never
/// aborts the batch.
#[hotpath::measure]
pub fn run_batch(
    inputs: Vec<Arc<InputBuffer>>,
    algorithms: &[BatchAlgorithm],
    skip_if_larger_or_equal: bool,
) -> Vec<BatchOutcome> {
    let groups = prepare_groups(inputs, algorithms);
    let (mut order, mut outcomes): (Vec<u32>, Vec<BatchOutcome>) = groups
        .into_par_iter()
        .flat_map(|group| {
            group.files.into_par_iter().map_with(
                Compressors::default(),
                move |compressors, file| {
                    (
                        file.result_index,
                        run_one(
                            compressors,
                            group.config,
                            file.input,
                            skip_if_larger_or_equal,
                        ),
                    )
                },
            )
        })
        .unzip();

    scatter_in_place(&mut outcomes, &mut order);
    outcomes
}

/// Move every `data[i]` to index `order[i]`. `order` is a permutation of
/// `0..data.len()`, used as scratch and left as the identity permutation.
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
fn run_one(
    compressors: &mut Compressors,
    config: &BatchAlgorithm,
    input: Arc<InputBuffer>,
    skip_if_larger_or_equal: bool,
) -> BatchOutcome {
    let input_len = input.len();
    let algorithm = config.algorithm;
    // Finishing this task releases its shared source handle.
    let result = catch_unwind(AssertUnwindSafe(|| {
        compressors.compress(
            config.algorithm,
            config.level,
            config.window_bits,
            config.section_size,
            &input,
        )
    }))
    .unwrap_or_else(|_| {
        // A panicking encoder may contain an unfinished stream.
        *compressors = Compressors::default();
        Err(Error::CompressionPanicked(algorithm))
    });

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
                simd_brotli::BrotliDecompress(&mut { input }, &mut out).expect("brotli decode");
                out
            }
            Algorithm::Zstd => zstd::stream::decode_all(input).expect("zstd decode"),
        }
    }

    fn text_fixture(seed: usize) -> Vec<u8> {
        format!("export const value{seed} = {seed};\n")
            .repeat(200 + seed * 7)
            .into_bytes()
    }

    fn config(algorithm: Algorithm, level: u32) -> BatchAlgorithm {
        BatchAlgorithm {
            algorithm,
            level,
            window_bits: None,
            section_size: None,
        }
    }

    fn algorithms() -> [BatchAlgorithm; 4] {
        [
            config(Algorithm::Gzip, 1),
            config(Algorithm::Brotli, 4),
            config(Algorithm::Zstd, 3),
            config(Algorithm::Gzip, 9),
        ]
    }

    fn run_batch_on_pool(
        inputs: Vec<Arc<InputBuffer>>,
        algorithms: &[BatchAlgorithm],
        threads: usize,
        skip_if_larger_or_equal: bool,
    ) -> Vec<BatchOutcome> {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("build pool")
            .install(|| run_batch(inputs, algorithms, skip_if_larger_or_equal))
    }

    #[test]
    fn batch_preserves_file_then_configuration_order() {
        let inputs: Vec<_> = [7, 0, 3, 3, 1]
            .into_iter()
            .map(|seed| Arc::new(text_fixture(seed)))
            .chain([Arc::new(Vec::new())])
            .collect();
        let algorithms = algorithms();
        let outcomes = run_batch_on_pool(inputs.clone(), &algorithms, 4, false);
        assert_eq!(outcomes.len(), inputs.len() * algorithms.len());
        for (file_outcomes, input) in outcomes.chunks(algorithms.len()).zip(&inputs) {
            for (outcome, config) in file_outcomes.iter().zip(&algorithms) {
                assert!(outcome.error.is_none());
                assert!(!outcome.skipped);
                assert_eq!(decompress(config.algorithm, &outcome.data), **input);
                // The two gzip levels have identical algorithm metadata.
                let fresh = crate::compress::compress(
                    config.algorithm,
                    config.level,
                    config.window_bits,
                    config.section_size,
                    input,
                )
                .expect("compress");
                assert_eq!(outcome.data, fresh);
            }
        }
    }

    #[test]
    fn batch_is_deterministic_across_thread_counts() {
        let inputs: Vec<_> = (0..24).map(|i| Arc::new(text_fixture(i))).collect();
        let algorithms = algorithms();
        let single = run_batch_on_pool(inputs.clone(), &algorithms, 1, false);
        for threads in [2, 4, 8] {
            let multi = run_batch_on_pool(inputs.clone(), &algorithms, threads, false);
            assert_eq!(single.len(), multi.len());
            for (a, b) in single.iter().zip(multi.iter()) {
                assert_eq!(a.data, b.data, "output differs with {threads} threads");
            }
        }
    }

    #[test]
    fn skip_if_larger_or_equal_marks_incompressible_items() {
        let input = Arc::new(vec![1u8, 2, 3, 4]);
        let algorithms = algorithms();
        let outcomes = run_batch_on_pool(vec![Arc::clone(&input)], &algorithms, 0, true);
        for outcome in outcomes {
            assert!(outcome.skipped);
            assert!(outcome.data.is_empty());
            assert!(outcome.error.is_none());
        }
        let outcomes = run_batch_on_pool(vec![Arc::clone(&input)], &algorithms, 0, false);
        for outcome in outcomes {
            assert!(!outcome.skipped);
            assert!(outcome.data.len() > input.len());
        }
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
    fn scatter_restores_every_permutation() {
        for n in 1..=6usize {
            let factorial: usize = (1..=n).product();
            for rank in 0..factorial {
                let mut order = permutation(n, rank);
                let mut data: Vec<_> = order.iter().map(|i| i * 10).collect();
                scatter_in_place(&mut data, &mut order);
                assert_eq!(data, (0..n as u32).map(|i| i * 10).collect::<Vec<_>>());
                assert_eq!(order, (0..n as u32).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn schedules_brotli_then_zstd_then_gzip_largest_first() {
        let algorithms = algorithms();
        let inputs = [20, 50, 10]
            .into_iter()
            .map(|len| Arc::new(vec![0u8; len]))
            .collect();
        let groups = prepare_groups(inputs, &algorithms);
        let scheduled: Vec<_> = groups
            .iter()
            .map(|group| {
                (
                    group.config.algorithm,
                    group.config.level,
                    group
                        .files
                        .iter()
                        .map(|file| file.input.len())
                        .collect::<Vec<_>>(),
                    group
                        .files
                        .iter()
                        .map(|file| file.result_index)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            scheduled,
            vec![
                (Algorithm::Brotli, 4, vec![50, 20, 10], vec![5, 1, 9]),
                (Algorithm::Zstd, 3, vec![50, 20, 10], vec![6, 2, 10]),
                (Algorithm::Gzip, 1, vec![50, 20, 10], vec![4, 0, 8]),
                (Algorithm::Gzip, 9, vec![50, 20, 10], vec![7, 3, 11]),
            ]
        );
    }

    #[test]
    fn shared_input_survives_a_failed_task_and_is_released_after_the_last_task() {
        let expected = text_fixture(1);
        let input = Arc::new(expected.clone());
        let weak = Arc::downgrade(&input);
        let failed = run_one(
            &mut Compressors::default(),
            &config(Algorithm::Brotli, 99),
            Arc::clone(&input),
            false,
        );
        assert!(failed.error.is_some());
        assert!(weak.upgrade().is_some());

        let algorithms = algorithms();
        let outcomes = run_batch_on_pool(vec![input], &algorithms, 3, false);
        assert!(weak.upgrade().is_none());
        for (outcome, config) in outcomes.iter().zip(&algorithms) {
            assert!(outcome.error.is_none());
            assert_eq!(decompress(config.algorithm, &outcome.data), expected);
        }
    }

    #[test]
    fn single_failure_does_not_abort_batch() {
        let inputs = vec![Arc::new(text_fixture(0)), Arc::new(text_fixture(1))];
        let algorithms = [config(Algorithm::Gzip, 6), config(Algorithm::Zstd, 99)];
        let outcomes = run_batch_on_pool(inputs, &algorithms, 0, false);
        assert_eq!(outcomes.len(), 4);
        for file_outcomes in outcomes.chunks(2) {
            assert!(file_outcomes[0].error.is_none());
            assert!(!file_outcomes[0].data.is_empty());
            assert!(matches!(
                file_outcomes[1].error,
                Some(Error::InvalidLevel {
                    algorithm: Algorithm::Zstd,
                    level: 99,
                    ..
                })
            ));
            assert!(file_outcomes[1].data.is_empty());
            assert!(!file_outcomes[1].skipped);
        }
    }

    #[test]
    fn empty_files_or_algorithms_produce_no_results() {
        assert!(run_batch(Vec::new(), &algorithms(), false).is_empty());
        assert!(run_batch(vec![Arc::new(text_fixture(0))], &[], false).is_empty());
        assert!(run_batch(Vec::new(), &[], false).is_empty());
    }
}
