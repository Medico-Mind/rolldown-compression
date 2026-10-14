---
"@medicomind/rolldown-compression": minor
---

Migrate zstd encoding to `mzstd`, a pure-Rust zstd encoder.

- **Inputs up to 4 MiB compress to the same bytes as before.** `mzstd`'s single-threaded encoder is byte-identical to zstd 1.5.7.
- **Larger inputs are split across the worker pool**, like brotli's sections: 4 MiB segments, each compressed by its own task into one standard Zstandard frame. The bytes differ from a single stream's, at about +0.05% size at level 19, and as with brotli they do not depend on core count or `concurrency`.
- **Splitting costs little below level 18 and a lot from level 18 up.** Every segment after the first first re-reads the input before it so its matches can reach back: an eighth of the window up to level 17 (256 KiB at level 3, 1 MiB at level 17), but the whole 8 MiB window for the `btultra` strategies of levels 18–22. A 20 MiB file at level 19 therefore costs about 2.4x the CPU of a single stream and finishes at best about 1.7x sooner. Measured on 18 threads with the benchmark fixtures, the three largest files alone finished 2.5x faster at level 3, 2.75x at level 9 and 1.32x at level 19. On the full 202-file set at level 19, the result ranged from 10% slower to 17% faster across runs, because there the extra work competes with the small files for cores, and peak RSS rose from about 1.9 GB to 2.2–2.8 GB.
- **The frame header is unchanged**, so the window stays within the 8 MiB limit browsers enforce for `Content-Encoding: zstd` at levels 1–19.
- **Levels 20–22 are capped at an 8 MiB window.** Their default 32–128 MiB windows made browsers reject any `.zst` file above 8 MiB (RFC 9659 limits the `zstd` content coding to 8 MiB). Files up to 8 MiB are unchanged.
- **No C toolchain is needed for zstd anymore**, including the wasm32 build. The C `zstd` crate is now a dev-dependency only, used as the reference decoder in round-trip tests.
