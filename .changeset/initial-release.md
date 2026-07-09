---
'@medicomind/rolldown-compression': minor
---

Initial release.

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
