//! Zstd-specific compression.

use std::cell::RefCell;

/// Reusable zstd compressor carried across the items a single rayon worker
/// handles.
///
/// A zstd context at the levels used here owns tens of megabytes of match
/// tables; keeping one per worker avoids reallocating them for every file.
/// `i32::MIN` marks a fresh context whose level is not yet configured
/// (validated levels are all above it).
struct ZstdContext {
    level: i32,
    compressor: zstd::bulk::Compressor<'static>,
}

impl Default for ZstdContext {
    fn default() -> Self {
        Self {
            level: i32::MIN,
            compressor: zstd::bulk::Compressor::default(),
        }
    }
}

thread_local! {
    static CONTEXT: RefCell<ZstdContext> = Default::default();
}

#[hotpath::measure(label = "compress_zstd")]
pub fn compress(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
    CONTEXT.with_borrow_mut(|context| {
        let level = level as i32;
        if context.level != level {
            context
                .compressor
                .set_compression_level(level)
                .map_err(|err| format!("zstd compression failed: {err}"))?;
            context.level = level;
        }
        context
            .compressor
            .compress(input)
            .map_err(|err| format!("zstd compression failed: {err}"))
    })
}
