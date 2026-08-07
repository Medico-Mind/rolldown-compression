use std::cell::RefCell;

/// Reusable zstd compressor carried across the items a single rayon worker
/// handles.
///
/// A zstd context at the levels used here owns tens of megabytes of match
/// tables; keeping one per worker avoids reallocating them for every file.
/// `i32::MIN` marks a fresh context whose level is not yet configured
/// (validated levels are all above it).
pub struct ZstdContext {
    pub level: i32,
    pub compressor: zstd::bulk::Compressor<'static>,
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
    pub static CONTEXT: RefCell<ZstdContext> = Default::default();
}
