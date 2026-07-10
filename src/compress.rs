//! Compression algorithm implementations.
//!
//! Pure Rust module with no napi dependency so it can be unit tested with
//! plain `cargo test`.

use std::cell::RefCell;
use std::io::Write;

use brotli::enc::threading::{Owned, SendAlloc};
use brotli::enc::{
    BrotliEncoderMaxCompressedSize, BrotliEncoderMaxCompressedSizeMulti, BrotliEncoderParams,
    SliceWrapper, StandardAlloc, UnionHasher,
};

/// Supported compression algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Gzip,
    Brotli,
    Zstd,
}

/// Default brotli window size (log2), matching `BROTLI_DEFAULT_WINDOW`.
pub const BROTLI_DEFAULT_WINDOW_BITS: u32 = 22;

/// Inputs at least this large are compressed with a multi-threaded brotli
/// job; below it a cross-file rayon batch already keeps all cores busy and
/// splitting would only cost ratio.
const BROTLI_MULTI_THRESHOLD: usize = 2 * 1024 * 1024;
/// Target section size per brotli worker thread. Sections much smaller than
/// the window lose too many cross-section matches.
const BROTLI_MULTI_SECTION: usize = 1024 * 1024;
const BROTLI_MULTI_MAX_THREADS: usize = 4;

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

/// Compress `input` with the given algorithm and level.
///
/// `window_bits` is only used by brotli and ignored by other algorithms.
pub fn compress(
    algorithm: Algorithm,
    level: u32,
    window_bits: Option<u32>,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    algorithm.validate_level(level)?;
    let mut output = match algorithm {
        Algorithm::Gzip => compress_gzip(level, input),
        Algorithm::Brotli => {
            let window_bits = window_bits.unwrap_or(BROTLI_DEFAULT_WINDOW_BITS);
            validate_window_bits(window_bits)?;
            compress_brotli(level, window_bits, input)
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

fn compress_brotli(quality: u32, window_bits: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    let params = BrotliEncoderParams {
        quality: quality as i32,
        lgwin: window_bits as i32,
        size_hint: input.len(),
        ..Default::default()
    };
    if input.len() >= BROTLI_MULTI_THRESHOLD {
        compress_brotli_multi(&params, input)
    } else {
        compress_brotli_single(&params, input)
    }
}

fn compress_brotli_single(params: &BrotliEncoderParams, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(BrotliEncoderMaxCompressedSize(input.len()));
    let mut reader = input;
    brotli::BrotliCompress(&mut reader, &mut output, params)
        .map_err(|err| format!("brotli compression failed: {err}"))?;
    Ok(output)
}

/// Owned input for `compress_multi`, which shares the buffer across worker
/// threads and therefore cannot borrow it.
struct HeapSlice(Vec<u8>);

impl SliceWrapper<u8> for HeapSlice {
    fn slice(&self) -> &[u8] {
        &self.0
    }
}

/// Compress one large input on several threads by splitting it into
/// ~[`BROTLI_MULTI_SECTION`] sections.
///
/// The thread count is a pure function of the input size, never of batch
/// concurrency, so a given (input, level, window) always produces identical
/// bytes. Sectioning costs a fraction of a percent of ratio versus a single
/// stream, in exchange for finishing the large files that dominate a batch
/// tail several times faster.
fn compress_brotli_multi(params: &BrotliEncoderParams, input: &[u8]) -> Result<Vec<u8>, String> {
    let num_threads = (input.len() / BROTLI_MULTI_SECTION).clamp(2, BROTLI_MULTI_MAX_THREADS);
    let mut output = vec![0u8; BrotliEncoderMaxCompressedSizeMulti(input.len(), num_threads)];
    let mut alloc_per_thread: Vec<_> = (0..num_threads)
        .map(|_| SendAlloc::new(StandardAlloc::default(), UnionHasher::Uninit))
        .collect();
    let written = brotli::enc::compress_multi(
        params,
        &mut Owned::new(HeapSlice(input.to_vec())),
        &mut output,
        &mut alloc_per_thread,
    )
    .map_err(|err| format!("brotli compression failed: {err:?}"))?;
    output.truncate(written);
    Ok(output)
}

fn compress_zstd(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    // A zstd context at the levels used here owns tens of megabytes of match
    // tables; keeping one per worker thread avoids reallocating them for
    // every file. `i32::MIN` marks a fresh context whose level is not yet
    // configured (validated levels are all above it).
    thread_local! {
        static CONTEXT: RefCell<(i32, zstd::bulk::Compressor<'static>)> =
            RefCell::new((i32::MIN, zstd::bulk::Compressor::default()));
    }
    CONTEXT.with(|cell| {
        let mut entry = cell.borrow_mut();
        let (current_level, compressor) = &mut *entry;
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
            let compressed =
                compress(algorithm, algorithm.default_level(), None, &input).expect("compress");
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
                compress(algorithm, algorithm.default_level(), None, &[]).expect("compress");
            assert_eq!(decompress(algorithm, &compressed), Vec::<u8>::new());
        }
    }

    #[test]
    fn round_trips_single_byte() {
        for algorithm in ALGORITHMS {
            let compressed =
                compress(algorithm, algorithm.default_level(), None, &[0x42]).expect("compress");
            assert_eq!(decompress(algorithm, &compressed), vec![0x42]);
        }
    }

    #[test]
    fn round_trips_incompressible_data() {
        let input = pseudo_random(256 * 1024);
        for algorithm in ALGORITHMS {
            let compressed =
                compress(algorithm, algorithm.default_level(), None, &input).expect("compress");
            assert_eq!(decompress(algorithm, &compressed), input);
        }
    }

    #[test]
    fn round_trips_highly_compressible_data() {
        let input = vec![0u8; 1024 * 1024];
        for algorithm in ALGORITHMS {
            let compressed =
                compress(algorithm, algorithm.default_level(), None, &input).expect("compress");
            assert!(compressed.len() < input.len() / 100);
            assert_eq!(decompress(algorithm, &compressed), input);
        }
    }

    #[test]
    fn round_trips_large_brotli_inputs_via_multithreaded_path() {
        // Sized to cross BROTLI_MULTI_THRESHOLD and exercise compress_multi.
        // Moderate qualities keep the debug-build test runtime reasonable;
        // the sectioning machinery is identical at every quality.
        let compressible = b"export const value = 42; // padding padding\n".repeat(52_000);
        assert!(compressible.len() >= BROTLI_MULTI_THRESHOLD);
        for level in [5, 9] {
            let compressed =
                compress(Algorithm::Brotli, level, None, &compressible).expect("compress");
            assert!(compressed.len() < compressible.len());
            assert_eq!(decompress(Algorithm::Brotli, &compressed), compressible);
        }

        let incompressible = pseudo_random(BROTLI_MULTI_THRESHOLD + 12_345);
        let compressed = compress(Algorithm::Brotli, 9, None, &incompressible).expect("compress");
        assert_eq!(decompress(Algorithm::Brotli, &compressed), incompressible);
    }

    #[test]
    fn multithreaded_brotli_is_deterministic() {
        let input = b"function chunk(a, b) { return a + b; }\n".repeat(58_000);
        assert!(input.len() >= BROTLI_MULTI_THRESHOLD);
        let first = compress(Algorithm::Brotli, 5, None, &input).expect("compress");
        let second = compress(Algorithm::Brotli, 5, None, &input).expect("compress");
        assert_eq!(first, second);
    }

    #[test]
    fn honors_level_bounds() {
        let input = b"hello world".repeat(1000);
        for algorithm in ALGORITHMS {
            let (min, max) = algorithm.level_range();
            for level in [min, max] {
                let compressed = compress(algorithm, level, None, &input).expect("compress");
                assert_eq!(decompress(algorithm, &compressed), input);
            }
        }
    }

    #[test]
    fn higher_levels_compress_no_worse() {
        let input = b"abcdefghij klmnopqrst 0123456789 ".repeat(5000);
        for algorithm in ALGORITHMS {
            let (min, max) = algorithm.level_range();
            let low = compress(algorithm, min.max(1), None, &input).unwrap();
            let high = compress(algorithm, max, None, &input).unwrap();
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
        assert!(compress(Algorithm::Gzip, 10, None, b"x").is_err());
        assert!(compress(Algorithm::Brotli, 12, None, b"x").is_err());
        assert!(compress(Algorithm::Zstd, 0, None, b"x").is_err());
        assert!(compress(Algorithm::Zstd, 23, None, b"x").is_err());
    }

    #[test]
    fn rejects_invalid_window_bits() {
        assert!(compress(Algorithm::Brotli, 11, Some(9), b"x").is_err());
        assert!(compress(Algorithm::Brotli, 11, Some(25), b"x").is_err());
        assert!(compress(Algorithm::Brotli, 11, Some(10), b"x").is_ok());
        assert!(compress(Algorithm::Brotli, 11, Some(24), b"x").is_ok());
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
