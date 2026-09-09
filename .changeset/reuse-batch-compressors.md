---
"@medicomind/rolldown-compression": patch
---

Build compression groups directly from algorithm settings and files sorted by size, scheduling Brotli, Zstd, then gzip with larger files first. Reuse Brotli and Zstd compressors within parallel file iterators and release their scratch memory when processing completes instead of retaining it in thread-local storage, while preserving result order.
