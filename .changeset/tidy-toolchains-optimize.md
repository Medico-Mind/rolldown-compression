---
'@medicomind/rolldown-compression': patch
---

Faster prebuilt Linux binaries through deeper build optimization:

- The `x86_64-linux-gnu` binding is now additionally optimized with LLVM BOLT: after the PGO build, the binary is instrumented, retrained on the compression workload, and re-laid-out post-link (basic-block reordering, function splitting, ICF).
- PGO now also covers the C dependencies (zstd, mimalloc), not just the Rust code, on the Linux targets: the CI installs a clang matching rustc's LLVM major and instruments the C sources with `-fprofile-generate`/`-fprofile-use` alongside rustc's `-Cprofile-generate`/`-Cprofile-use`, so both languages train and optimize from one merged profile. The new `--c-pgo` flag in `scripts/pgo/build.mjs` verifies the clang/rustc LLVM majors match and safely falls back to Rust-only PGO when they don't.
- All Linux targets now compile their C sources with clang (previously host gcc on `x86_64-linux-gnu` and musl).

No API or behavior changes — the published bindings are just faster on Linux.
