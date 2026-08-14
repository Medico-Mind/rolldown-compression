use simd_brotli::enc::{Allocator, BrotliAlloc, SliceWrapper, SliceWrapperMut};
use std::any::Any;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

static BROTLI_ALLOCATOR: LazyLock<CachingAlloc> = LazyLock::new(CachingAlloc::default);

#[inline]
pub fn brotli_allocator() -> CachingAlloc {
    BROTLI_ALLOCATOR.clone()
}

/// Cloneable Brotli allocator backed by a shared cache of typed allocations.
///
/// Brotli consumes an allocator when it creates an encoder, so the cache lives
/// behind the handle rather than in the handle itself. This also lets all
/// sections of a multi-stream compression reuse the same pool without moving
/// allocations between rayon workers.
#[derive(Clone, Default)]
pub struct CachingAlloc {
    cache: Arc<Mutex<ObjectCache>>,
}

#[derive(Default)]
struct ObjectCache {
    by_type: anymap3::Map<dyn Any + Send>,
}

pub struct CachedMemory<T>(Vec<T>);

impl<T> Default for CachedMemory<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> SliceWrapper<T> for CachedMemory<T> {
    fn slice(&self) -> &[T] {
        &self.0
    }
}

impl<T> SliceWrapperMut<T> for CachedMemory<T> {
    fn slice_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

impl CachingAlloc {
    fn cache(&self) -> MutexGuard<'_, ObjectCache> {
        // A panic in an encoder must not permanently disable a worker's cache.
        self.cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl ObjectCache {
    fn objects<T: Send + 'static>(&mut self) -> &mut Vec<Vec<T>> {
        self.by_type.entry().or_default()
    }
}

impl<T> Allocator<T> for CachingAlloc
where
    T: Clone + Default + Send + 'static,
{
    type AllocatedMemory = CachedMemory<T>;

    fn alloc_cell(&mut self, len: usize) -> Self::AllocatedMemory {
        if len == 0 {
            return CachedMemory::default();
        }

        let cached = {
            let mut cache = self.cache();
            let objects = cache.objects::<T>();
            let index = objects
                .iter()
                .enumerate()
                .filter(|(_, object)| object.capacity() >= len && len < object.capacity() * 2)
                .min_by_key(|(_, object)| object.capacity())
                .map(|(index, _)| index);
            index.map(|index| objects.swap_remove(index))
        };

        let mut data = cached.unwrap_or_else(|| Vec::with_capacity(len));
        data.clear();
        data.resize_with(len, T::default);
        CachedMemory(data)
    }

    fn free_cell(&mut self, data: Self::AllocatedMemory) {
        let data = data.0;
        if data.capacity() == 0 {
            return;
        }

        let capacity = data.capacity();
        let mut cache = self.cache();
        let objects = cache.objects::<T>();

        // When a workload grows, replace one allocation that can no longer
        // satisfy it. Thus each type retains its high-water object count, not
        // one extra generation of objects for every input size encountered.
        if let Some((index, _)) = objects
            .iter()
            .enumerate()
            .filter(|(_, cached)| cached.capacity() < capacity)
            .min_by_key(|(_, cached)| cached.capacity())
        {
            objects.swap_remove(index);
        }
        objects.push(data);
    }
}

impl BrotliAlloc for CachingAlloc {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_allocators_share_and_reuse_typed_memory() {
        let mut first = CachingAlloc::default();
        let mut second = first.clone();

        let mut memory = <CachingAlloc as Allocator<u32>>::alloc_cell(&mut first, 1024);
        let pointer = memory.slice_mut().as_mut_ptr();
        memory.slice_mut().fill(123);
        <CachingAlloc as Allocator<u32>>::free_cell(&mut first, memory);

        let memory = <CachingAlloc as Allocator<u32>>::alloc_cell(&mut second, 512);
        assert_eq!(memory.slice().as_ptr(), pointer);
        assert_eq!(memory.slice(), &[0; 512]);
    }

    #[test]
    fn larger_objects_replace_undersized_cache_entries() {
        let mut alloc = CachingAlloc::default();
        let small = <CachingAlloc as Allocator<u8>>::alloc_cell(&mut alloc, 64);
        <CachingAlloc as Allocator<u8>>::free_cell(&mut alloc, small);

        let large = <CachingAlloc as Allocator<u8>>::alloc_cell(&mut alloc, 128);
        <CachingAlloc as Allocator<u8>>::free_cell(&mut alloc, large);

        let cache = alloc.cache();
        let objects = cache.by_type.get::<Vec<Vec<u8>>>().unwrap();
        assert_eq!(objects.len(), 1);
        assert!(objects[0].capacity() >= 128);
    }
}
