use simd_brotli::enc::BrotliAlloc;
use simd_brotli::{Allocator, SliceWrapper, SliceWrapperMut};

#[derive(Default)]
pub struct WrapBox<T>(Vec<T>);

impl<T> SliceWrapper<T> for WrapBox<T> {
    fn slice(&self) -> &[T] {
        &self.0
    }
}

impl<T> SliceWrapperMut<T> for WrapBox<T> {
    fn slice_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct DefaultAlloc;

impl<T: Clone + Default> Allocator<T> for DefaultAlloc {
    type AllocatedMemory = WrapBox<T>;
    fn alloc_cell(&mut self, len: usize) -> WrapBox<T> {
        WrapBox(vec![T::default(); len])
    }
    fn free_cell(&mut self, _data: WrapBox<T>) {}
}

impl BrotliAlloc for DefaultAlloc {}
