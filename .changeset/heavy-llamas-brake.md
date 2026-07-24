---
"@medicomind/rolldown-compression": major
---

Require Node.js v22.14.0 or newer by targeting Node-API 10, and switch the native addon to mimalloc as the global allocator.

**Why Node-API 10:** the addon previously targeted Node-API 8 to keep Node 18 support. Node 18 is end-of-life, so we now build against Node-API 10 (shipped in Node v22.14.0+). This lets napi-rs use the newest runtime APIs — notably cheaper Buffer creation from existing ArrayBuffers, which matters for a plugin whose entire output is compressed Buffers — instead of compatibility fallbacks for old runtimes.

**Why mimalloc:** batch compression is allocation-heavy and runs on every core through the rayon worker pool, where the system allocator becomes a contention point — especially musl's malloc on Alpine, which serializes heavily under multi-threaded load. mimalloc's per-thread heaps remove that contention and speed up the many short-lived encoder/buffer allocations. On musl targets it is built with `local_dynamic_tls` so the dlopen-ed `.node` addon does not exhaust musl's static TLS space (the "cannot allocate memory in static TLS block" failure). The wasm32-wasip1 fallback build keeps the default allocator.
