//! Compression algorithm implementations.
//!
//! This module holds only the algorithm-agnostic surface: the [`Algorithm`]
//! enum, level validation, and the [`compress`] entry point that dispatches
//! into the per-algorithm `inner_*` submodules.
//!
//! Unit tested with plain `cargo test`: the only napi type used is the
//! [`InputBuffer`] alias, which switches to `Vec<u8>` under test so the test
//! harness never references Node-API symbols (they only exist inside a
//! Node.js process).

mod inner_brotli;
mod inner_gzip;
mod inner_zstd;

use std::fmt::Display;
use std::str::FromStr;

pub use inner_brotli::{BROTLI_DEFAULT_WINDOW_BITS, validate_section_size, validate_window_bits};

/// Owned compression input: the napi buffer handed over the FFI boundary in
/// production, a plain `Vec<u8>` under `cargo test`. Both hand out `&[u8]`
/// and are `Send + Sync`, which lets brotli's worker pool share the buffer
/// across threads without copying it.
#[cfg(not(test))]
pub type InputBuffer = napi::bindgen_prelude::Buffer;
/// Owned compression input; `Vec<u8>` stands in for the napi buffer in tests.
#[cfg(test)]
pub type InputBuffer = Vec<u8>;

/// Supported compression algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Gzip,
    Brotli,
    Zstd,
}

impl Algorithm {
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
                self
            ));
        }
        Ok(())
    }
}

impl FromStr for Algorithm {
    type Err = String;

    /// Parse a canonical algorithm name coming over the FFI boundary.
    ///
    /// Alias normalization (`gz`, `br`, `zstandard`, ...) happens in the
    /// TypeScript layer; the native module only accepts canonical names.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "gzip" => Ok(Self::Gzip),
            "brotli" => Ok(Self::Brotli),
            "zstd" => Ok(Self::Zstd),
            other => Err(format!(
                "unknown algorithm `{other}`, expected one of: gzip, brotli, zstd"
            )),
        }
    }
}

impl Display for Algorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gzip => f.write_str("gzip"),
            Self::Brotli => f.write_str("brotli"),
            Self::Zstd => f.write_str("zstd"),
        }
    }
}

/// Compress `input` with the given algorithm and level.
///
/// `window_bits` and `section_size` are only used by brotli and ignored by
/// other algorithms.
///
/// `state` carries the reusable per-worker scratch state; see [`CompressState`].
///
/// Takes ownership of the input so brotli's multithreaded path can share it
/// across worker threads without copying; it is dropped as soon as
/// compression finishes.
#[hotpath::measure]
pub fn compress(
    algorithm: Algorithm,
    level: u32,
    window_bits: Option<u32>,
    section_size: Option<u32>,
    input: InputBuffer,
) -> Result<Vec<u8>, String> {
    algorithm.validate_level(level)?;
    let mut output = match algorithm {
        Algorithm::Gzip => inner_gzip::compress(level, input.as_ref()),
        Algorithm::Brotli => inner_brotli::compress(level, window_bits, section_size, input),
        Algorithm::Zstd => inner_zstd::compress(level, input.as_ref()),
    }?;
    // Output buffers are sized for the worst case, so compressible input
    // leaves most of the capacity unused; results are held until the JS side
    // drains the batch, so hand back right-sized buffers.
    output.shrink_to_fit();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const ALGORITHMS: [Algorithm; 3] = [Algorithm::Gzip, Algorithm::Brotli, Algorithm::Zstd];

    /// Shadows [`super::compress`] with a fresh [`CompressState`] per call, the
    /// one-shot equivalent of what the scheduler reuses across a worker's
    /// items. Shared with the `inner_*` submodules' own test modules.
    pub(super) fn compress(
        algorithm: Algorithm,
        level: u32,
        window_bits: Option<u32>,
        section_size: Option<u32>,
        input: InputBuffer,
    ) -> Result<Vec<u8>, String> {
        super::compress(algorithm, level, window_bits, section_size, input)
    }

    /// Shared with the `inner_*` submodules' own test modules.
    pub(super) fn decompress(algorithm: Algorithm, input: &[u8]) -> Vec<u8> {
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
                simd_brotli::BrotliDecompress(&mut reader, &mut out).expect("brotli decode");
                out
            }
            Algorithm::Zstd => zstd::stream::decode_all(input).expect("zstd decode"),
        }
    }

    pub(super) fn pseudo_random(len: usize) -> Vec<u8> {
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
            let compressed = compress(
                algorithm,
                algorithm.default_level(),
                None,
                None,
                input.clone(),
            )
            .expect("compress");
            assert!(
                compressed.len() < input.len(),
                "{} should shrink repetitive input",
                algorithm
            );
            assert_eq!(
                decompress(algorithm, &compressed),
                input,
                "{} round-trip mismatch",
                algorithm
            );
        }
    }

    #[test]
    fn round_trips_empty_buffer() {
        for algorithm in ALGORITHMS {
            let compressed = compress(algorithm, algorithm.default_level(), None, None, Vec::new())
                .expect("compress");
            assert_eq!(decompress(algorithm, &compressed), Vec::<u8>::new());
        }
    }

    #[test]
    fn round_trips_single_byte() {
        for algorithm in ALGORITHMS {
            let compressed = compress(algorithm, algorithm.default_level(), None, None, vec![0x42])
                .expect("compress");
            assert_eq!(decompress(algorithm, &compressed), vec![0x42]);
        }
    }

    #[test]
    fn round_trips_incompressible_data() {
        let input = pseudo_random(256 * 1024);
        for algorithm in ALGORITHMS {
            let compressed = compress(
                algorithm,
                algorithm.default_level(),
                None,
                None,
                input.clone(),
            )
            .expect("compress");
            assert_eq!(decompress(algorithm, &compressed), input);
        }
    }

    #[test]
    fn round_trips_highly_compressible_data() {
        let input = vec![0u8; 1024 * 1024];
        for algorithm in ALGORITHMS {
            let compressed = compress(
                algorithm,
                algorithm.default_level(),
                None,
                None,
                input.clone(),
            )
            .expect("compress");
            assert!(compressed.len() < input.len() / 100);
            assert_eq!(decompress(algorithm, &compressed), input);
        }
    }

    #[test]
    fn honors_level_bounds() {
        let input = b"hello world".repeat(1000);
        for algorithm in ALGORITHMS {
            let (min, max) = algorithm.level_range();
            for level in [min, max] {
                let compressed =
                    compress(algorithm, level, None, None, input.clone()).expect("compress");
                assert_eq!(decompress(algorithm, &compressed), input);
            }
        }
    }

    #[test]
    fn higher_levels_compress_no_worse() {
        let input = b"abcdefghij klmnopqrst 0123456789 ".repeat(5000);
        for algorithm in ALGORITHMS {
            let (min, max) = algorithm.level_range();
            let low = compress(algorithm, min.max(1), None, None, input.clone()).unwrap();
            let high = compress(algorithm, max, None, None, input.clone()).unwrap();
            assert!(
                high.len() <= low.len(),
                "{}: level {max} produced {} bytes vs {} at min level",
                algorithm,
                high.len(),
                low.len()
            );
        }
    }

    #[test]
    fn rejects_invalid_levels() {
        assert!(compress(Algorithm::Gzip, 10, None, None, b"x".to_vec()).is_err());
        assert!(compress(Algorithm::Brotli, 12, None, None, b"x".to_vec()).is_err());
        assert!(compress(Algorithm::Zstd, 0, None, None, b"x".to_vec()).is_err());
        assert!(compress(Algorithm::Zstd, 23, None, None, b"x".to_vec()).is_err());
    }

    #[test]
    fn parses_canonical_names_only() {
        assert_eq!("gzip".parse::<Algorithm>().unwrap(), Algorithm::Gzip);
        assert_eq!("brotli".parse::<Algorithm>().unwrap(), Algorithm::Brotli);
        assert_eq!("zstd".parse::<Algorithm>().unwrap(), Algorithm::Zstd);
        assert!("gz".parse::<Algorithm>().is_err());
        assert!("lzma".parse::<Algorithm>().is_err());
    }
}
