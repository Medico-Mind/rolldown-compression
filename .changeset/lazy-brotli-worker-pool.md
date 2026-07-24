---
"@medicomind/rolldown-compression": minor
---

Lazier brotli worker pool and retuned multithreading defaults

- The brotli worker pool is now initialized lazily: worker threads are only spawned on the first input large enough to take the multithreaded path, instead of on every rayon worker thread up front. Small workloads no longer pay the thread-spawn cost. Also fixed a lazy-init bug where the worker-pool path could silently fall back to the single-stream encoder.
- Retuned multithreaded brotli defaults: the default `sectionSize` grew from 1 MiB to 4 MiB (matching the default 22-bit window), and inputs now need to be at least 4× `sectionSize` (16 MiB at the default, previously 2 MiB) to be split across the worker pool. Inputs between 2 MiB and 16 MiB are now compressed as a single stream, which improves their compression ratio; a cross-file batch keeps all cores busy in that range anyway.
- Broadened the PGO training corpus (SVG sprites, vendor-sized HTML/CSS, a 17 MiB payload, and a dedicated worker-pool batch) so shipped binaries are profile-optimized for the retuned multithreaded path as well; benchmark CI now measures the same PGO+BOLT builds that are released, and native tests also run on aarch64 Linux.
