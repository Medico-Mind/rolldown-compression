---
"@medicomind/rolldown-compression": patch
---

Internal: split the compression core into per-algorithm submodules (`compress/inner_{brotli,gzip,zstd}.rs`), and add opt-in [hotpath](https://hotpath.rs) profiling of the batch, per-file and per-algorithm paths behind the crate's `hotpath` / `hotpath-alloc` cargo features. Both are off in every published build — the macros expand to noops and pull in no third-party dependencies — so released bindings are unaffected at compile time and at runtime.
