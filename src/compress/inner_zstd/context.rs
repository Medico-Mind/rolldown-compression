/// Reusable zstd compressor carried across the items a rayon partition
/// handles.
///
/// A zstd context at the levels used here owns tens of megabytes of match
/// tables; retaining them in the partition avoids rebuilding them for each file.
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
