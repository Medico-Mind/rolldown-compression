use crate::compress::inner_brotli::BROTLI_BUFFER_SIZE;
use std::cell::{LazyCell, RefCell};

thread_local! {
    pub static BROTLI_BUFFER: RefCell<LazyCell<BrotliBuf>> = Default::default();
}

/// Scratch space for `BrotliCompressCustomAlloc`, split into an input and an
/// output half, carried across the items a single rayon worker handles.
pub struct BrotliBuf {
    inner: Vec<u8>,
}

impl BrotliBuf {
    pub fn split(&mut self) -> (&mut [u8], &mut [u8]) {
        self.inner.split_at_mut(BROTLI_BUFFER_SIZE)
    }
}

impl Default for BrotliBuf {
    fn default() -> Self {
        BrotliBuf {
            inner: vec![0; BROTLI_BUFFER_SIZE * 2],
        }
    }
}
