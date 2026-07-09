//! Compression algorithm implementations.
//!
//! Pure Rust module with no napi dependency so it can be unit tested with
//! plain `cargo test`.

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
    match algorithm {
        Algorithm::Gzip => compress_gzip(level, input),
        Algorithm::Brotli => {
            let window_bits = window_bits.unwrap_or(BROTLI_DEFAULT_WINDOW_BITS);
            validate_window_bits(window_bits)?;
            compress_brotli(level, window_bits, input)
        }
        Algorithm::Zstd => compress_zstd(level, input),
    }
}

fn compress_gzip(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = flate2::write::GzEncoder::new(
        Vec::with_capacity(input.len() / 3 + 64),
        flate2::Compression::new(level),
    );
    encoder
        .write_all(input)
        .and_then(|_| encoder.finish())
        .map_err(|err| format!("gzip compression failed: {err}"))
}

fn compress_brotli(quality: u32, window_bits: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    let params = brotli::enc::BrotliEncoderParams {
        quality: quality as i32,
        lgwin: window_bits as i32,
        size_hint: input.len(),
        ..Default::default()
    };
    let mut output = Vec::with_capacity(input.len() / 3 + 64);
    let mut reader = input;
    brotli::BrotliCompress(&mut reader, &mut output, &params)
        .map_err(|err| format!("brotli compression failed: {err}"))?;
    Ok(output)
}

fn compress_zstd(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    zstd::stream::encode_all(input, level as i32)
        .map_err(|err| format!("zstd compression failed: {err}"))
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
