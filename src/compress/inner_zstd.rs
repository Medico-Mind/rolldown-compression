//! Zstd-specific compression.

use crate::error::Error;

mod context;
#[hotpath::measure(label = "compress_zstd")]
pub fn compress(level: u32, input: &[u8]) -> Result<Vec<u8>, Error> {
    context::CONTEXT.with_borrow_mut(|context| {
        let level = level as i32;
        if context.level != level {
            context
                .compressor
                .set_compression_level(level)
                .map_err(Error::Zstd)?;
            context.level = level;
        }
        context.compressor.compress(input).map_err(Error::Zstd)
    })
}
