---
"@medicomind/rolldown-compression": minor
---

Run brotli's sectioned path on rayon, and make its output deterministic.

- Large brotli inputs are now split across rayon's global thread pool instead of brotli's own `WorkerPool`. Sections share the threads that already run the per-file batch, so there are no dedicated OS threads to spawn, no pool to check out and hand back, and no oversubscription when many large files compress at once. Joining a section works rayon's queue instead of parking the worker, so a batch whose every worker sits inside a large-file compression cannot starve itself.
- The section count is now derived from the input length alone. It previously depended on the worker pool's thread budget, which made compressed bytes for inputs past the multithreading threshold vary across machines and `concurrency` settings; the same input now produces the same bytes everywhere.
- `sectionSize` defaults to two windows (`2^(windowBits + 1)` bytes) instead of one, and inputs take the multithreaded path from twice `sectionSize` instead of four times. At the default window that is 8 MiB sections with multithreading still starting at 16 MiB, so large files are cut into fewer, larger sections and keep more cross-section matches. Compressed bytes for brotli inputs above the threshold differ from 2.1.7; gzip and zstd output is unchanged.
