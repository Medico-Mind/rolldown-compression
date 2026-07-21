//! Compression algorithm implementations.
//!
//! Pure Rust module with no napi dependency so it can be unit tested with
//! plain `cargo test`.

#[cfg(not(windows))]
use brotli::enc::threading::{Owned, SendAlloc};
use brotli::enc::{BrotliEncoderMaxCompressedSize, BrotliEncoderParams};
#[cfg(not(windows))]
use brotli::enc::{
    BrotliEncoderMaxCompressedSizeMulti, CompressionThreadResult, SliceWrapper, StandardAlloc,
    UnionHasher, WorkerPool, compress_worker_pool, new_work_pool,
};
use std::cell::RefCell;
use std::io::Write;

/// Supported compression algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Gzip,
    Brotli,
    Zstd,
}

/// Default brotli window size (log2), matching `BROTLI_DEFAULT_WINDOW`.
pub const BROTLI_DEFAULT_WINDOW_BITS: u32 = 22;

/// Default target section size per brotli worker thread. Sections much
/// smaller than the window lose too many cross-section matches.
pub const BROTLI_DEFAULT_SECTION_SIZE: u32 = 1024 * 1024;
#[cfg(not(windows))]
const BROTLI_MIN_THREADS: usize = 2;
#[cfg(not(windows))]
const BROTLI_MAX_THREADS: usize = 4;

impl Algorithm {
    /// Parse a canonical algorithm name coming over the FFI boundary.
    ///
    /// Alias normalization (`gz`, `br`, `zstandard`, ...) happens in the
    /// TypeScript layer; the native module only accepts canonical names.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "gzip" => Ok(Self::Gzip),
            "brotli" => Ok(Self::Brotli),
            "zstd" => Ok(Self::Zstd),
            other => Err(format!(
                "unknown algorithm `{other}`, expected one of: gzip, brotli, zstd"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Brotli => "brotli",
            Self::Zstd => "zstd",
        }
    }

    pub fn default_level(self) -> u32 {
        match self {
            Self::Gzip => 6,
            Self::Brotli => 11,
            Self::Zstd => 19,
        }
    }

    pub fn level_range(self) -> (u32, u32) {
        match self {
            Self::Gzip => (0, 9),
            Self::Brotli => (0, 11),
            Self::Zstd => (1, 22),
        }
    }

    pub fn validate_level(self, level: u32) -> Result<(), String> {
        let (min, max) = self.level_range();
        if level < min || level > max {
            return Err(format!(
                "invalid {} level {level}: expected {min}..={max}",
                self.name()
            ));
        }
        Ok(())
    }
}

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

/// Compress `input` with the given algorithm and level.
///
/// `window_bits` and `section_size` are only used by brotli and ignored by
/// other algorithms.
pub fn compress(
    algorithm: Algorithm,
    level: u32,
    window_bits: Option<u32>,
    section_size: Option<u32>,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    algorithm.validate_level(level)?;
    let mut output = match algorithm {
        Algorithm::Gzip => compress_gzip(level, input),
        Algorithm::Brotli => {
            let window_bits = window_bits.unwrap_or(BROTLI_DEFAULT_WINDOW_BITS);
            validate_window_bits(window_bits)?;
            let section_size = section_size.unwrap_or(BROTLI_DEFAULT_SECTION_SIZE);
            validate_section_size(section_size)?;
            compress_brotli(level, window_bits, section_size as usize, input)
        }
        Algorithm::Zstd => compress_zstd(level, input),
    }?;
    // Output buffers are sized for the worst case, so compressible input
    // leaves most of the capacity unused; results are held until the JS side
    // drains the batch, so hand back right-sized buffers.
    output.shrink_to_fit();
    Ok(output)
}

fn compress_gzip(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    // deflate's worst case is stored blocks: 5 bytes per 16 KiB block plus
    // the 18-byte gzip container. len/1000 over-covers the block overhead,
    // so the buffer never reallocates mid-compression.
    let bound = input.len() + input.len() / 1000 + 64;
    let mut encoder =
        flate2::write::GzEncoder::new(Vec::with_capacity(bound), flate2::Compression::new(level));
    encoder
        .write_all(input)
        .and_then(|_| encoder.finish())
        .map_err(|err| format!("gzip compression failed: {err}"))
}

/// Owned input for `compress_worker_pool`, which shares the buffer across
/// worker threads and therefore cannot borrow it.
#[cfg(not(windows))]
struct HeapSlice(Vec<u8>);

#[cfg(not(windows))]
impl SliceWrapper<u8> for HeapSlice {
    fn slice(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(not(windows))]
type BrotliWorkerPool = WorkerPool<
    CompressionThreadResult<StandardAlloc>,
    UnionHasher<StandardAlloc>,
    StandardAlloc,
    (HeapSlice, BrotliEncoderParams),
>;

#[cfg(not(windows))]
thread_local! {
    /// Per-worker-thread brotli pool plus the section budget it was sized
    /// for. Not available on Windows: `WorkerPool::drop` joins its threads,
    /// and a `thread_local` is dropped in a TLS destructor, which on Windows
    /// runs under the loader lock — the joined thread cannot exit without
    /// that same lock, deadlocking the process (rust-lang/rust#74875).
    static BROTLI_WORKER_POOL: RefCell<BrotliWorkerPool> = {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(BROTLI_MIN_THREADS, BROTLI_MAX_THREADS);
        // The calling thread compresses the last section itself, so a budget
        // of `threads` sections needs only `threads - 1` pool workers.
        RefCell::new(new_work_pool(threads.saturating_sub(1)))
    };
}

/// Compress large inputs by splitting them into ~`section_size` sections
/// spread over the per-thread worker pool; smaller inputs stay in one
/// section on the calling thread.
///
/// Inputs at least twice `section_size` are split; below that a cross-file
/// rayon batch already keeps all cores busy and splitting would only cost
/// ratio. The section count depends on the pool's thread budget, so output
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
fn compress_brotli(
    quality: u32,
    window_bits: u32,
    section_size: usize,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    let params = BrotliEncoderParams {
        quality: quality as i32,
        lgwin: window_bits as i32,
        size_hint: input.len(),
        ..Default::default()
    };
    #[cfg(not(windows))]
    if input.len() >= 2 * section_size {
        let num_sections =
            (input.len() / section_size).clamp(BROTLI_MIN_THREADS, BROTLI_MAX_THREADS);
        return BROTLI_WORKER_POOL
            .with_borrow_mut(|pool| compress_brotli_pooled(&params, num_sections, pool, input));
    }
    #[cfg(windows)]
    let _ = section_size;
    compress_brotli_single(&params, input)
}

fn compress_brotli_single(params: &BrotliEncoderParams, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(BrotliEncoderMaxCompressedSize(input.len()));
    let mut reader = input;
    brotli::BrotliCompress(&mut reader, &mut output, params)
        .map_err(|err| format!("brotli compression failed: {err}"))?;
    Ok(output)
}

#[cfg(not(windows))]
fn compress_brotli_pooled(
    params: &BrotliEncoderParams,
    num_sections: usize,
    pool: &mut BrotliWorkerPool,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    let mut output = vec![0u8; BrotliEncoderMaxCompressedSizeMulti(input.len(), num_sections)];
    let mut alloc_per_thread: Vec<_> = (0..num_sections)
        .map(|_| SendAlloc::new(StandardAlloc::default(), UnionHasher::Uninit))
        .collect();
    let written = compress_worker_pool(
        params,
        &mut Owned::new(HeapSlice(input.to_vec())),
        &mut output,
        &mut alloc_per_thread,
        pool,
    )
    .map_err(|err| format!("brotli compression failed: {err:?}"))?;
    output.truncate(written);
    Ok(output)
}

// A zstd context at the levels used here owns tens of megabytes of match
// tables; keeping one per worker thread avoids reallocating them for
// every file. `i32::MIN` marks a fresh context whose level is not yet
// configured (validated levels are all above it).
thread_local! {
    static ZSTD_CONTEXT: RefCell<(i32, zstd::bulk::Compressor<'static>)> =
        RefCell::new((i32::MIN, zstd::bulk::Compressor::default()));
}

fn compress_zstd(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    ZSTD_CONTEXT.with_borrow_mut(|(current_level, compressor)| {
        let level = level as i32;
        if *current_level != level {
            compressor
                .set_compression_level(level)
                .map_err(|err| format!("zstd compression failed: {err}"))?;
            *current_level = level;
        }
        compressor
            .compress(input)
            .map_err(|err| format!("zstd compression failed: {err}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const ALGORITHMS: [Algorithm; 3] = [Algorithm::Gzip, Algorithm::Brotli, Algorithm::Zstd];

    /// Default section size and the multi-section threshold derived from it,
    /// mirroring the on-the-fly computation in `compress_brotli`.
    const DEFAULT_SECTION_SIZE: usize = BROTLI_DEFAULT_SECTION_SIZE as usize;
    const DEFAULT_MULTI_THRESHOLD: usize = 2 * DEFAULT_SECTION_SIZE;

    fn decompress(algorithm: Algorithm, input: &[u8]) -> Vec<u8> {
        match algorithm {
            Algorithm::Gzip => {
                let mut decoder = flate2::read::GzDecoder::new(input);
                let mut out = Vec::new();
                decoder.read_to_end(&mut out).expect("gzip decode");
                out
            }
            Algorithm::Brotli => {
                let mut out = Vec::new();
                let mut reader = input;
                brotli::BrotliDecompress(&mut reader, &mut out).expect("brotli decode");
                out
            }
            Algorithm::Zstd => zstd::stream::decode_all(input).expect("zstd decode"),
        }
    }

    fn pseudo_random(len: usize) -> Vec<u8> {
        // xorshift-based deterministic noise: effectively incompressible.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state & 0xff) as u8
            })
            .collect()
    }

    #[test]
    fn round_trips_all_algorithms() {
        let input = b"the quick brown fox jumps over the lazy dog".repeat(100);
        for algorithm in ALGORITHMS {
            let compressed = compress(algorithm, algorithm.default_level(), None, None, &input)
                .expect("compress");
            assert!(
                compressed.len() < input.len(),
                "{} should shrink repetitive input",
                algorithm.name()
            );
            assert_eq!(
                decompress(algorithm, &compressed),
                input,
                "{} round-trip mismatch",
                algorithm.name()
            );
        }
    }

    #[test]
    fn round_trips_empty_buffer() {
        for algorithm in ALGORITHMS {
            let compressed =
                compress(algorithm, algorithm.default_level(), None, None, &[]).expect("compress");
            assert_eq!(decompress(algorithm, &compressed), Vec::<u8>::new());
        }
    }

    #[test]
    fn round_trips_single_byte() {
        for algorithm in ALGORITHMS {
            let compressed = compress(algorithm, algorithm.default_level(), None, None, &[0x42])
                .expect("compress");
            assert_eq!(decompress(algorithm, &compressed), vec![0x42]);
        }
    }

    #[test]
    fn round_trips_incompressible_data() {
        let input = pseudo_random(256 * 1024);
        for algorithm in ALGORITHMS {
            let compressed = compress(algorithm, algorithm.default_level(), None, None, &input)
                .expect("compress");
            assert_eq!(decompress(algorithm, &compressed), input);
        }
    }

    #[test]
    fn round_trips_highly_compressible_data() {
        let input = vec![0u8; 1024 * 1024];
        for algorithm in ALGORITHMS {
            let compressed = compress(algorithm, algorithm.default_level(), None, None, &input)
                .expect("compress");
            assert!(compressed.len() < input.len() / 100);
            assert_eq!(decompress(algorithm, &compressed), input);
        }
    }

    #[test]
    fn round_trips_large_brotli_inputs_via_multithreaded_path() {
        // Sized to cross DEFAULT_MULTI_THRESHOLD and exercise the worker pool.
        // Moderate qualities keep the debug-build test runtime reasonable;
        // the sectioning machinery is identical at every quality.
        let compressible = b"export const value = 42; // padding padding\n".repeat(52_000);
        assert!(compressible.len() >= DEFAULT_MULTI_THRESHOLD);
        for level in [5, 9] {
            let compressed =
                compress(Algorithm::Brotli, level, None, None, &compressible).expect("compress");
            assert!(compressed.len() < compressible.len());
            assert_eq!(decompress(Algorithm::Brotli, &compressed), compressible);
        }

        let incompressible = pseudo_random(DEFAULT_MULTI_THRESHOLD + 12_345);
        let compressed =
            compress(Algorithm::Brotli, 9, None, None, &incompressible).expect("compress");
        assert_eq!(decompress(Algorithm::Brotli, &compressed), incompressible);
    }

    #[test]
    #[cfg(not(windows))]
    fn worker_pool_round_trips_multi_section_inputs() {
        // The global pool's section budget depends on which test initializes
        // it first, so pin a dedicated pool to guarantee multi-section
        // coverage: 4 sections need 3 pool workers plus the calling thread.
        let input = b"export const value = 42; // padding padding\n".repeat(104_000);
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
            compress_brotli_pooled(&params, num_sections, &mut pool, &input).expect("compress");
        assert!(compressed.len() < input.len());
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn multithreaded_brotli_is_deterministic() {
        let input = b"function chunk(a, b) { return a + b; }\n".repeat(58_000);
        assert!(input.len() >= DEFAULT_MULTI_THRESHOLD);
        let first = compress(Algorithm::Brotli, 5, None, None, &input).expect("compress");
        let second = compress(Algorithm::Brotli, 5, None, None, &input).expect("compress");
        assert_eq!(first, second);
    }

    #[test]
    fn honors_level_bounds() {
        let input = b"hello world".repeat(1000);
        for algorithm in ALGORITHMS {
            let (min, max) = algorithm.level_range();
            for level in [min, max] {
                let compressed = compress(algorithm, level, None, None, &input).expect("compress");
                assert_eq!(decompress(algorithm, &compressed), input);
            }
        }
    }

    #[test]
    fn higher_levels_compress_no_worse() {
        let input = b"abcdefghij klmnopqrst 0123456789 ".repeat(5000);
        for algorithm in ALGORITHMS {
            let (min, max) = algorithm.level_range();
            let low = compress(algorithm, min.max(1), None, None, &input).unwrap();
            let high = compress(algorithm, max, None, None, &input).unwrap();
            assert!(
                high.len() <= low.len(),
                "{}: level {max} produced {} bytes vs {} at min level",
                algorithm.name(),
                high.len(),
                low.len()
            );
        }
    }

    #[test]
    fn rejects_invalid_levels() {
        assert!(compress(Algorithm::Gzip, 10, None, None, b"x").is_err());
        assert!(compress(Algorithm::Brotli, 12, None, None, b"x").is_err());
        assert!(compress(Algorithm::Zstd, 0, None, None, b"x").is_err());
        assert!(compress(Algorithm::Zstd, 23, None, None, b"x").is_err());
    }

    #[test]
    fn rejects_invalid_window_bits() {
        assert!(compress(Algorithm::Brotli, 11, Some(9), None, b"x").is_err());
        assert!(compress(Algorithm::Brotli, 11, Some(25), None, b"x").is_err());
        assert!(compress(Algorithm::Brotli, 11, Some(10), None, b"x").is_ok());
        assert!(compress(Algorithm::Brotli, 11, Some(24), None, b"x").is_ok());
    }

    #[test]
    fn rejects_invalid_section_size() {
        assert!(compress(Algorithm::Brotli, 11, None, Some(0), b"x").is_err());
        assert!(compress(Algorithm::Brotli, 11, None, Some(1), b"x").is_ok());
    }

    #[test]
    fn honors_custom_section_size() {
        // 256 KiB sections push a ~1 MB input through the multithreaded path
        // that the default section size would compress single-threaded.
        let input = b"export const value = 42; // padding padding\n".repeat(24_000);
        let section_size = 256 * 1024_u32;
        assert!(input.len() >= 2 * section_size as usize);
        assert!(input.len() < DEFAULT_MULTI_THRESHOLD);
        let compressed =
            compress(Algorithm::Brotli, 5, None, Some(section_size), &input).expect("compress");
        assert!(compressed.len() < input.len());
        assert_eq!(decompress(Algorithm::Brotli, &compressed), input);
    }

    #[test]
    fn parses_canonical_names_only() {
        assert_eq!(Algorithm::parse("gzip").unwrap(), Algorithm::Gzip);
        assert_eq!(Algorithm::parse("brotli").unwrap(), Algorithm::Brotli);
        assert_eq!(Algorithm::parse("zstd").unwrap(), Algorithm::Zstd);
        assert!(Algorithm::parse("gz").is_err());
        assert!(Algorithm::parse("lzma").is_err());
    }
}
