---
"@medicomind/rolldown-compression": patch
---

Lower per-batch overhead in the native binding:

- The rayon thread pool is now built once, up front on the calling thread, and the whole batch — input preparation, compression, and result marshalling — runs inside it. Previously the pool was rebuilt inside the async worker on every `compress` call, and only the compression step ran on it. `concurrency: 0` (the default) still means one thread per logical CPU; a pool that cannot be created is now reported as an error instead of silently falling back to the global rayon pool.
- Batch preparation and result collection are parallelized with rayon instead of running as serial iterator passes, so large batches spend less time on the single worker thread before and after the actual compression.
- Enabled the napi-rs fast path (`dyn-symbols` + `node_version_detect`), cutting FFI call overhead on supported Node versions.
- Internal: `Algorithm` now implements `FromStr`/`Display` instead of inherent `parse`/`name` methods.

No API or behavior changes — compressed output is unchanged.
