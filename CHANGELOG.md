# @medicomind/rolldown-compression

## 2.3.6

### Patch Changes

- [#36](https://github.com/Medico-Mind/rolldown-compression/pull/36) [`0b67e41`](https://github.com/Medico-Mind/rolldown-compression/commit/0b67e41be90e4ae8adfe9c6232e5b1be7ef29cc2) Thanks [@Mnwa](https://github.com/Mnwa)! - Reuse Brotli encoder allocations through a process-wide cache to reduce allocation count and volume across concurrent compression jobs.

## 2.3.5

### Patch Changes

- [`6baafca`](https://github.com/Medico-Mind/rolldown-compression/commit/6baafca502c6fcdefc3895a1424b1f7ebc1a4da0) Thanks [@Mnwa](https://github.com/Mnwa)! - bump simd brotli version

- [`d3276cf`](https://github.com/Medico-Mind/rolldown-compression/commit/d3276cf20ee0202881a7709b5be28589398bc52d) Thanks [@Mnwa](https://github.com/Mnwa)! - Compress brotli big files in a scope

## 2.3.4

### Patch Changes

- [#33](https://github.com/Medico-Mind/rolldown-compression/pull/33) [`80cc193`](https://github.com/Medico-Mind/rolldown-compression/commit/80cc193a03466fd6057090dcf5c4f84e464b193a) Thanks [@Mnwa](https://github.com/Mnwa)! - Migrate to simd brotli

## 2.3.3

### Patch Changes

- [`4a0fe1a`](https://github.com/Medico-Mind/rolldown-compression/commit/4a0fe1a3165f7afdbcdd976d81949ca59638835f) Thanks [@Mnwa](https://github.com/Mnwa)! - Refactor compression. Split code into modules

- [`4a0fe1a`](https://github.com/Medico-Mind/rolldown-compression/commit/4a0fe1a3165f7afdbcdd976d81949ca59638835f) Thanks [@Mnwa](https://github.com/Mnwa)! - Reduce brotli memory usage for small files

## 2.3.2

### Patch Changes

- [`15f6ce3`](https://github.com/Medico-Mind/rolldown-compression/commit/15f6ce3c4b7bc4818ee65f97237181d0678316ff) Thanks [@Mnwa](https://github.com/Mnwa)! - Return thread_local and use max_len 1 to improve concurrency

## 2.3.1

### Patch Changes

- [`505b8a9`](https://github.com/Medico-Mind/rolldown-compression/commit/505b8a91f06acb2a971a845b5d174ef034c27c41) Thanks [@Mnwa](https://github.com/Mnwa)! - Rename CompressMeta to CompressState. Use lazy cell for state items

- [`08cbbbb`](https://github.com/Medico-Mind/rolldown-compression/commit/08cbbbbe5bf493b5390359403c5bd1b4abe1a2bc) Thanks [@Mnwa](https://github.com/Mnwa)! - Sort tasks to concat same compression level closer.

## 2.3.0

### Minor Changes

- [#28](https://github.com/Medico-Mind/rolldown-compression/pull/28) [`f738722`](https://github.com/Medico-Mind/rolldown-compression/commit/f7387220e91fcf9fd3eb27b0c969891a8eefd446) Thanks [@Mnwa](https://github.com/Mnwa)! - Build the x86_64 binaries for the x86-64-v2 baseline.

  - The prebuilt `x86_64` bindings (linux-gnu, linux-musl, darwin and windows-msvc) are now compiled with `-C target-cpu=x86-64-v2`, and the C dependencies on the linux and darwin targets with the matching `-march=x86-64-v2`. That lets the compressors use SSE3 through SSE4.2, POPCNT and CMPXCHG16B directly instead of the 2003-era baseline rustc and cc default to.
  - The requirement that follows: the `x86_64` binaries need a CPU from Intel Nehalem (2008) or AMD Bulldozer (2011) onwards. Every x86_64 machine and cloud instance still in service clears that bar, but a build host older than those, or a VM pinned to an emulated pre-Nehalem CPU model, will now fault on load instead of running. `aarch64` and `wasm32-wasi` builds are untouched.

### Patch Changes

- [#28](https://github.com/Medico-Mind/rolldown-compression/pull/28) [`f738722`](https://github.com/Medico-Mind/rolldown-compression/commit/f7387220e91fcf9fd3eb27b0c969891a8eefd446) Thanks [@Mnwa](https://github.com/Mnwa)! - Internal: hold the per-worker compression scratch in rayon's own state, and trim the zstd build.

  - The brotli scratch buffers and the zstd compressor moved out of `thread_local!` storage and into a `CompressMeta` value that rayon creates once per worker via `map_init` and threads through the batch by `&mut`. The state is reused across the files a worker handles exactly as before, but no thread-local storage is involved: the brotli scratch is a plain inline array again rather than a heap `Vec` working around glibc's static TLS surplus in a `dlopen`ed `cdylib`, and neither buffer depends on the lifetime of the thread that first touched it.
  - zstd is built with `default-features = false`, dropping the legacy-format decoder (v0.1–0.7), the array APIs and the dictionary builder — none of which this plugin calls — from the compiled C, and the `wasm32-wasi` build adds `no_asm` so it stops trying to assemble the x86 decompression routines.

## 2.2.0

### Minor Changes

- [`e170791`](https://github.com/Medico-Mind/rolldown-compression/commit/e170791f0fbb3c78666604245054131c286272a8) Thanks [@Mnwa](https://github.com/Mnwa)! - Run brotli's sectioned path on rayon, and scale it to the whole pool.

  - Large brotli inputs are now split across rayon's global thread pool instead of brotli's own `WorkerPool`. Sections share the threads that already run the per-file batch, so there are no dedicated OS threads to spawn, no pool to check out and hand back, and no oversubscription when many large files compress at once. Joining a section works rayon's queue instead of parking the worker, so a batch whose every worker sits inside a large-file compression cannot starve itself.
  - A large input is now cut into one section per full `sectionSize`, capped at one section per worker thread, where the count was previously capped at four regardless of how wide the pool was. Very large bundles now finish on every core instead of a quarter of them: on 100 MiB of real JS at quality 11 with 18 workers, the file compresses in 6.1s instead of 19.0s (3.1x) for 0.1% more output — a tenth of the ratio that splitting the file at all already costs. Going past one section per worker was measured to be slower, not faster, as the extra sections queue behind each other.
  - Because that cap follows the pool, a file big enough to reach it (larger than `threads * sectionSize`, i.e. 8 MiB per thread at the default window) is split according to the machine's core count or the `concurrency` setting, so its compressed bytes can differ between machines. Inputs below the cap are split by length alone and compress identically everywhere, as does everything gzip and zstd produce. If you need byte-identical brotli output for very large files across a heterogeneous fleet, either set `concurrency` so every machine builds a pool of the same width, or raise `sectionSize` far enough that the section count stays under the cap.
  - `sectionSize` defaults to two windows (`2^(windowBits + 1)` bytes) instead of one, and inputs take the multithreaded path from twice `sectionSize` instead of four times. At the default window that is 8 MiB sections with multithreading still starting at 16 MiB, so large files are cut into fewer, larger sections and keep more cross-section matches. Compressed bytes for brotli inputs above the threshold differ from 2.1.7; gzip and zstd output is unchanged.

### Patch Changes

- [`e170791`](https://github.com/Medico-Mind/rolldown-compression/commit/e170791f0fbb3c78666604245054131c286272a8) Thanks [@Mnwa](https://github.com/Mnwa)! - Internal: split the compression core into per-algorithm submodules (`compress/inner_{brotli,gzip,zstd}.rs`), and add opt-in [hotpath](https://hotpath.rs) profiling of the batch, per-file and per-algorithm paths behind the crate's `hotpath` / `hotpath-alloc` cargo features. Both are off in every published build — the macros expand to noops and pull in no third-party dependencies — so released bindings are unaffected at compile time and at runtime.

## 2.1.8

### Patch Changes

- [`f15e4b4`](https://github.com/Medico-Mind/rolldown-compression/commit/f15e4b4ed61eec6ea449ef61a08518b6cf5f5541) Thanks [@Mnwa](https://github.com/Mnwa)! - Reduce allocations in the native compression path: gzip now compresses through a buffered reader with a tighter worst-case output bound, and brotli reuses thread-local scratch buffers instead of allocating them per call.

- [`3ff01c1`](https://github.com/Medico-Mind/rolldown-compression/commit/3ff01c1272ee5c618b746f39b0e8493203206b29) Thanks [@Mnwa](https://github.com/Mnwa)! - Fix the published `wasm32-wasi` package manifest, which had drifted behind the napi-rs CLI: it no longer declares `cpu: ["wasm32"]` (the WASI binding runs on any host architecture), now declares `type: module` for its loaders, ships and points `types` at `rolldown-compression.wasi.d.cts`, and pins the `@emnapi/core` / `@emnapi/runtime` versions the binding is actually built against.

## 2.1.7

### Patch Changes

- [`addbfa5`](https://github.com/Medico-Mind/rolldown-compression/commit/addbfa564547d9553eef6c29633d4df6b0f44389) Thanks [@Mnwa](https://github.com/Mnwa)! - Fix build

## 2.1.6

### Patch Changes

- [`8b3049d`](https://github.com/Medico-Mind/rolldown-compression/commit/8b3049dd365856bd85c9553e4de79e35449b6769) Thanks [@Mnwa](https://github.com/Mnwa)! - Fix deploy miss index.js

## 2.1.5

### Patch Changes

- [`e7afef7`](https://github.com/Medico-Mind/rolldown-compression/commit/e7afef7d58f4c3b9f8355f2e723a20001c365d72) Thanks [@Mnwa](https://github.com/Mnwa)! - Fix dropping brotli worker pool when panic occured

## 2.1.4

### Patch Changes

- [`5f34992`](https://github.com/Medico-Mind/rolldown-compression/commit/5f34992b6603ed6b5ed9363db1c7b369ed9c7d13) Thanks [@Mnwa](https://github.com/Mnwa)! - Simplify batch processing. Remove sort and max_len from tasks preprocessing.

- [`a1dee03`](https://github.com/Medico-Mind/rolldown-compression/commit/a1dee03460cb514d519ac942e534393efc1d7978) Thanks [@Mnwa](https://github.com/Mnwa)! - Use global pool for brotli worker pools. Return compress multi for windows

## 2.1.3

### Patch Changes

- [`da23391`](https://github.com/Medico-Mind/rolldown-compression/commit/da23391ece86130432fe1b41d452ad65bac4cf75) Thanks [@Mnwa](https://github.com/Mnwa)! - Lower per-batch overhead in the native binding:

  - Every batch now runs on rayon's global thread pool instead of a private pool built per `compress` call, and the whole batch — input preparation, compression, and result marshalling — runs on it rather than just the compression step. A non-zero `concurrency` sizes that global pool the first time it is requested; because the global pool can only be built once per process, later batches asking for a different thread count keep the size established by the first one, and a pool that cannot be sized is warned about on stderr rather than failing the batch. `concurrency: 0` (the default) leaves rayon's default sizing, one thread per logical CPU.
  - Batch preparation and result collection are parallelized with rayon instead of running as serial iterator passes, so large batches spend less time on the single worker thread before and after the actual compression.
  - Enabled the napi-rs fast path (`dyn-symbols` + `node_version_detect`), cutting FFI call overhead on supported Node versions.
  - Internal: `Algorithm` now implements `FromStr`/`Display` instead of inherent `parse`/`name` methods.

  No API or behavior changes — compressed output is unchanged.

## 2.1.2

### Patch Changes

- [`6e0b693`](https://github.com/Medico-Mind/rolldown-compression/commit/6e0b693fd24338102876ecb4bb605ed37e1a3804) Thanks [@Mnwa](https://github.com/Mnwa)! - Default brotli `sectionSize` to one window (`2^windowBits` bytes) instead of a fixed 4 MiB, so a custom `windowBits` gets a matching section size. Unchanged at the default window of 22, where both are 4 MiB.

## 2.1.1

### Patch Changes

- [`2fa931e`](https://github.com/Medico-Mind/rolldown-compression/commit/2fa931efef1b6e58e48c0782aeed83aee6b9d26b) Thanks [@Mnwa](https://github.com/Mnwa)! - Fix windows warning and up deps

## 2.1.0

### Minor Changes

- [`a273ee7`](https://github.com/Medico-Mind/rolldown-compression/commit/a273ee7e17321963d75de2ccceaa76f93cb6b6cf) Thanks [@Mnwa](https://github.com/Mnwa)! - Lazier brotli worker pool and retuned multithreading defaults

  - The brotli worker pool is now initialized lazily: worker threads are only spawned on the first input large enough to take the multithreaded path, instead of on every rayon worker thread up front. Small workloads no longer pay the thread-spawn cost. Also fixed a lazy-init bug where the worker-pool path could silently fall back to the single-stream encoder.
  - Retuned multithreaded brotli defaults: the default `sectionSize` grew from 1 MiB to 4 MiB (matching the default 22-bit window), and inputs now need to be at least 4× `sectionSize` (16 MiB at the default, previously 2 MiB) to be split across the worker pool. Inputs between 2 MiB and 16 MiB are now compressed as a single stream, which improves their compression ratio; a cross-file batch keeps all cores busy in that range anyway.
  - Broadened the PGO training corpus (SVG sprites, vendor-sized HTML/CSS, a 17 MiB payload, and a dedicated worker-pool batch) so shipped binaries are profile-optimized for the retuned multithreaded path as well; benchmark CI now measures the same PGO+BOLT builds that are released, and native tests also run on aarch64 Linux.

## 2.0.1

### Patch Changes

- [`b3d6170`](https://github.com/Medico-Mind/rolldown-compression/commit/b3d61702cd9db4fb2f4d54c24419e50e1ea9a3fe) Thanks [@Mnwa](https://github.com/Mnwa)! - Faster prebuilt Linux binaries through deeper build optimization:

  - The `x86_64-linux-gnu` binding is now additionally optimized with LLVM BOLT: after the PGO build, the binary is instrumented, retrained on the compression workload, and re-laid-out post-link (basic-block reordering, function splitting, ICF).
  - PGO now also covers the C dependencies (zstd, mimalloc), not just the Rust code, on the Linux targets: the CI installs a clang matching rustc's LLVM major and instruments the C sources with `-fprofile-generate`/`-fprofile-use` alongside rustc's `-Cprofile-generate`/`-Cprofile-use`, so both languages train and optimize from one merged profile. The new `--c-pgo` flag in `scripts/pgo/build.mjs` verifies the clang/rustc LLVM majors match and safely falls back to Rust-only PGO when they don't.
  - All Linux targets now compile their C sources with clang (previously host gcc on `x86_64-linux-gnu` and musl).

  No API or behavior changes — the published bindings are just faster on Linux.

## 2.0.0

### Major Changes

- [`6728b63`](https://github.com/Medico-Mind/rolldown-compression/commit/6728b63535a9dadd590b759fdae781377cbef9a0) Thanks [@Mnwa](https://github.com/Mnwa)! - Require Node.js v22.14.0 or newer by targeting Node-API 10, and switch the native addon to mimalloc as the global allocator.

  **Why Node-API 10:** the addon previously targeted Node-API 8 to keep Node 18 support. Node 18 is end-of-life, so we now build against Node-API 10 (shipped in Node v22.14.0+). This lets napi-rs use the newest runtime APIs — notably cheaper Buffer creation from existing ArrayBuffers, which matters for a plugin whose entire output is compressed Buffers — instead of compatibility fallbacks for old runtimes.

  **Why mimalloc:** batch compression is allocation-heavy and runs on every core through the rayon worker pool, where the system allocator becomes a contention point — especially musl's malloc on Alpine, which serializes heavily under multi-threaded load. mimalloc's per-thread heaps remove that contention and speed up the many short-lived encoder/buffer allocations. On musl targets it is built with `local_dynamic_tls` so the dlopen-ed `.node` addon does not exhaust musl's static TLS space (the "cannot allocate memory in static TLS block" failure). The wasm32-wasip1 fallback build keeps the default allocator.

## 1.2.2

### Patch Changes

- [`c8748a1`](https://github.com/Medico-Mind/rolldown-compression/commit/c8748a126a5825390baec29dcd6bf2684158dc8a) Thanks [@Mnwa](https://github.com/Mnwa)! - Reuse napi buffers when process content

## 1.2.1

### Patch Changes

- [`62cadfa`](https://github.com/Medico-Mind/rolldown-compression/commit/62cadfaf974826ea19ffb108cdefc28068b2f674) Thanks [@Mnwa](https://github.com/Mnwa)! - Return thread local but remove it for windows

- [`47763fa`](https://github.com/Medico-Mind/rolldown-compression/commit/47763fa3c0d4f6fa0c1c1a08a8042e2e04d4ee6f) Thanks [@Mnwa](https://github.com/Mnwa)! - Fix windows deadlock

## 1.2.0

### Minor Changes

- [`31fcbe3`](https://github.com/Medico-Mind/rolldown-compression/commit/31fcbe3c3155e9d0e08b5474e184c0d34fe3ddfe) Thanks [@Mnwa](https://github.com/Mnwa)! - Move section size brotli to plugin config

### Patch Changes

- [`8788204`](https://github.com/Medico-Mind/rolldown-compression/commit/8788204ada0ba014d536b122aed787b7317cdda4) Thanks [@Mnwa](https://github.com/Mnwa)! - Reuse brotli thread pool for big files

## 1.1.0

### Minor Changes

- [`1d98d99`](https://github.com/Medico-Mind/rolldown-compression/commit/1d98d995567280c60194451818c7caffb7808bc6) Thanks [@Mnwa](https://github.com/Mnwa)! - Add `stream` option: compress from disk in `writeBundle` (order `post`) instead of in memory in `generateBundle`. Files are read on demand in bounded batches (`chunkSize` source bytes per batch, or 4 MB when `chunkSize` is 0), so the whole build is never buffered by the plugin and assets written to disk by other plugins' `writeBundle` hooks are compressed as well — removing the previous `generateBundle`-only limitation.

## 1.0.1

### Patch Changes

- [`6b1ad48`](https://github.com/Medico-Mind/rolldown-compression/commit/6b1ad4873aa83cde7fb6c038c609094045c901c5) Thanks [@Mnwa](https://github.com/Mnwa)! - Speed up compress batches processing

## 1.0.0

### Major Changes

- [`c3a02d2`](https://github.com/Medico-Mind/rolldown-compression/commit/c3a02d202d89e734f52766fe6d766780a9619605) Thanks [@Mnwa](https://github.com/Mnwa)! - Stabilize API

## 0.3.5

### Patch Changes

- [`f362b51`](https://github.com/Medico-Mind/rolldown-compression/commit/f362b51d6149caebb511d93445ae0bf17746c756) Thanks [@Mnwa](https://github.com/Mnwa)! - Add a wasm build

## 0.3.4

### Patch Changes

- [`0d8f332`](https://github.com/Medico-Mind/rolldown-compression/commit/0d8f332e30b182dfffbc68b2a912e4a892b3fe0e) Thanks [@Mnwa](https://github.com/Mnwa)! - Add multicompress and threaded compress for zstd and brotli

- [`25f3f8e`](https://github.com/Medico-Mind/rolldown-compression/commit/25f3f8eec15e6838ec64a2cb7eef4915f9e6a0c3) Thanks [@Mnwa](https://github.com/Mnwa)! - Add chunkSize option. It will reduce memory usage but increase time to compress.

- [`ad7ce4b`](https://github.com/Medico-Mind/rolldown-compression/commit/ad7ce4b26e4809c40e8286a9f9a66957e03d9bcd) Thanks [@Mnwa](https://github.com/Mnwa)! - Add random buffers and big files to train corpus

## 0.3.3

### Patch Changes

- [`09e6051`](https://github.com/Medico-Mind/rolldown-compression/commit/09e60515347263107017b720300bbddba7d6fa3f) Thanks [@Mnwa](https://github.com/Mnwa)! - Inject optional deps on a release time

## 0.3.2

### Patch Changes

- [`1def850`](https://github.com/Medico-Mind/rolldown-compression/commit/1def8503a2ffd8fc06c3a8bf394fad7e256734a5) Thanks [@Mnwa](https://github.com/Mnwa)! - Declare the platform binary packages in `optionalDependencies` so npm actually installs the native binding. 0.3.1 shipped without them, causing "Cannot find native binding" on import.

## 0.3.1

### Patch Changes

- [`88eee52`](https://github.com/Medico-Mind/rolldown-compression/commit/88eee5253615078ea71f58cebf6d27b412756049) Thanks [@Mnwa](https://github.com/Mnwa)! - Install llvm to wf

## 0.3.0

### Minor Changes

- [`0d0ef1a`](https://github.com/Medico-Mind/rolldown-compression/commit/0d0ef1a19e16394253a73c059e04e9f13df94f3d) Thanks [@Mnwa](https://github.com/Mnwa)! - Add a PGO and Bolt optimizations

## 0.2.0

### Minor Changes

- [`8c73f0c`](https://github.com/Medico-Mind/rolldown-compression/commit/8c73f0c725501fcf69ebbe3ea9a2d5264437d089) Thanks [@Mnwa](https://github.com/Mnwa)! - Update dependencies: TypeScript 7 toolchain, @types/node 26, zlib-rs 0.6.6.

### Patch Changes

- [`798219c`](https://github.com/Medico-Mind/rolldown-compression/commit/798219c059dc6c514912bbaea3844e3c0d09b447) Thanks [@Mnwa](https://github.com/Mnwa)! - Elide lifetimes

- [`996537c`](https://github.com/Medico-Mind/rolldown-compression/commit/996537c2948681e323aa0d67fdc077e5e2887df2) Thanks [@Mnwa](https://github.com/Mnwa)! - rollback ts to 7

## 0.1.0

### Minor Changes

- [`5307247`](https://github.com/Medico-Mind/rolldown-compression/commit/5307247e332a479672b6f91bfa76953922bd695e) Thanks [@Mnwa](https://github.com/Mnwa)! - Initial release.

  - Rolldown plugin compressing emitted assets with gzip, brotli and zstd.
  - Native Rust compression core (napi-rs v3 + rayon): one batched FFI call per
    build, parallel across files and algorithms, cost-aware scheduling,
    off-main-thread execution.
  - `compression()` / `defineAlgorithm()` API mirroring vite-plugin-compression2:
    include/exclude filters, threshold, filename patterns, deleteOriginalAssets,
    skipIfLargerOrEqual, concurrency, logLevel.
  - Eager option validation, re-compression guard, watch-mode no-op with
    `enableInWatchMode` opt-in.
  - Prebuilt binaries for darwin-arm64/x64, linux-x64/arm64-gnu, linux-x64-musl,
    win32-x64-msvc.

- [`66182be`](https://github.com/Medico-Mind/rolldown-compression/commit/66182be208983bbbcac805cf0bb1d2671f86fcd2) Thanks [@Mnwa](https://github.com/Mnwa)! - First release

### Patch Changes

- [`0fdd849`](https://github.com/Medico-Mind/rolldown-compression/commit/0fdd849328e71701b1aedadb677a82e6e75640a8) Thanks [@Mnwa](https://github.com/Mnwa)! - Set repo

Release notes are generated by [changesets](https://github.com/changesets/changesets); new sections are added here automatically when a release PR is merged.
