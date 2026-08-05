//! Zstd-specific compression.

use std::cell::RefCell;

// A zstd context at the levels used here owns tens of megabytes of match
// tables; keeping one per worker thread avoids reallocating them for
// every file. `i32::MIN` marks a fresh context whose level is not yet
// configured (validated levels are all above it).
thread_local! {
    static ZSTD_CONTEXT: RefCell<(i32, zstd::bulk::Compressor<'static>)> =
        RefCell::new((i32::MIN, zstd::bulk::Compressor::default()));
}

#[hotpath::measure(label = "compress_zstd")]
pub fn compress(level: u32, input: &[u8]) -> Result<Vec<u8>, String> {
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
