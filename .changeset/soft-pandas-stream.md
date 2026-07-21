---
"@medicomind/rolldown-compression": minor
---

Add `stream` option: compress from disk in `writeBundle` (order `post`) instead of in memory in `generateBundle`. Files are read on demand in bounded batches (`chunkSize` source bytes per batch, or 4 MB when `chunkSize` is 0), so the whole build is never buffered by the plugin and assets written to disk by other plugins' `writeBundle` hooks are compressed as well — removing the previous `generateBundle`-only limitation.
