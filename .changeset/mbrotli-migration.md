---
"@medicomind/rolldown-compression": minor
---

Migrate brotli encoding to `mbrotli`.

- **Compressed bytes change for brotli inputs above the split threshold.** The new encoder's sectioned stream trades more ratio for parallelism than the old one did: about 5% against a single stream (5.1% at 8 MiB, 5.8% at 16 MiB, 6.7% at 32 MiB, quality 11 at the default window), in exchange for finishing 3.2x, 6.2x and 10.9x faster on an 18-worker pool. Inputs below the threshold, and all gzip and zstd output, are unchanged.
- **Brotli output no longer depends on the machine.** How many sections an input is cut into follows `sectionSize` alone, and how many threads work through them is a scheduling decision that leaves the bytes alone. A given input and set of options now compress to identical bytes regardless of core count or `concurrency`, as gzip and zstd already did. The previous caveat about files larger than `threads * sectionSize` differing between machines no longer applies.
- **`sectionSize` is clamped to 64 KiB - 16 MiB**, the range the new encoder segments in. Its meaning, its `2^(windowBits + 1)` default and the `2 * sectionSize` split threshold are unchanged; previously valid values outside the range are clamped rather than rejected.
- **Large inputs now use the whole pool.** The section count no longer caps the threads that work on it, so a split file runs on every worker rather than one per section.
- **Each worker reuses its brotli encoder across files** instead of building one per file, capped by a 4 MiB retention budget. The win depends on consecutive files on a worker sharing an encoder shape: batches of similarly sized files compress about 28% faster in the per-file overhead, while batches of widely varying sizes are unchanged, because the window is re-derived per input and a shape change discards the workspace. The budget is what keeps the reuse from costing memory — at quality 11 a retained workspace is 2.3 MB at window 14 but 62.9 MB at window 22, so without it every worker would hold its high-water mark for the life of the process.
- `simd-brotli` is now a dev-dependency only; it is kept because `mbrotli` ships no decoder and the round-trip tests need one.

The brotli benchmark tables in the README predate this change and have not been re-measured.
