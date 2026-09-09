---
"@medicomind/rolldown-compression": patch
---

Group compression tasks by algorithm settings and reuse Brotli and Zstd compressors within parallel file iterators. Release compressor scratch memory when processing completes instead of retaining it in thread-local storage, while preserving result order.
