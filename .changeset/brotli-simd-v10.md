---
"@medicomind/rolldown-compression": patch
---

Update simd-brotli to v10, speeding up brotli compression. On a 36 MB production-shaped bundle corpus (62 files): quality 11 is ~5% faster (7518ms -> 7143ms) and quality 6 ~3% faster (111ms -> 108ms), with byte-identical output.
