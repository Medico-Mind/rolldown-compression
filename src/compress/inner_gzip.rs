//! Gzip-specific compression.

use std::io::Read;

/// Calculates the maximum upper bound of bytes required to safely hold
/// compressed data in the standard gzip format for a given input length.
///
/// This formula accounts for the worst-case DEFLATE overhead (~0.03%)
/// plus the standard 18-byte gzip header and trailer (12 bytes larger than zlib).
/// It assumes a minimal header with no optional metadata (like filenames).
fn compress_bound_formula(source_len: usize) -> usize {
    source_len + (source_len >> 12) + (source_len >> 14) + (source_len >> 25) + 25
}

#[hotpath::measure(label = "compress_gzip")]
pub fn compress(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(compress_bound_formula(input.len()));
    // Read-side encoder: every `read` deflates, so the wrapper reports real
    // compression work and compressed bytes out rather than buffered writes.
    let mut encoder = hotpath::io!(
        flate2::GzBuilder::new().buf_read(input, flate2::Compression::new(level)),
        label = "gzip-deflate"
    );
    encoder
        .read_to_end(&mut output)
        .map_err(|err| format!("gzip compression failed: {err}"))?;
    Ok(output)
}
