# @medicomind/rolldown-compression

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
