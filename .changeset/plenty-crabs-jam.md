---
"@medicomind/rolldown-compression": patch
---

Reduce allocations in the native compression path: gzip now compresses through a buffered reader with a tighter worst-case output bound, and brotli reuses thread-local scratch buffers instead of allocating them per call.
