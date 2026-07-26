---
"@medicomind/rolldown-compression": patch
---

Lower per-batch overhead in the native binding:

- Every batch now runs on rayon's global thread pool instead of a private pool built per `compress` call, and the whole batch — input preparation, compression, and result marshalling — runs on it rather than just the compression step. A non-zero `concurrency` sizes that global pool the first time it is requested; because the global pool can only be built once per process, later batches asking for a different thread count keep the size established by the first one, and a pool that cannot be sized is warned about on stderr rather than failing the batch. `concurrency: 0` (the default) leaves rayon's default sizing, one thread per logical CPU.
- Batch preparation and result collection are parallelized with rayon instead of running as serial iterator passes, so large batches spend less time on the single worker thread before and after the actual compression.
- Enabled the napi-rs fast path (`dyn-symbols` + `node_version_detect`), cutting FFI call overhead on supported Node versions.
- Internal: `Algorithm` now implements `FromStr`/`Display` instead of inherent `parse`/`name` methods.

No API or behavior changes — compressed output is unchanged.
