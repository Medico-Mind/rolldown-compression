//! Zstd-specific compression.

use crate::error::Error;

mod context;
pub(super) use context::ZstdContext;

#[hotpath::measure(label = "compress_zstd")]
pub fn compress(context: &mut ZstdContext, level: u32, input: &[u8]) -> Result<Vec<u8>, Error> {
    let level = level as i32;
    if context.level != level {
        context
            .compressor
            .set_compression_level(level)
            .map_err(Error::Zstd)?;
        context.level = level;
    }
    context.compressor.compress(input).map_err(Error::Zstd)
}
