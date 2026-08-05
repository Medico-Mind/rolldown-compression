---
"@medicomind/rolldown-compression": patch
---

Internal: hold the per-worker compression scratch in rayon's own state, and trim the zstd build.

- The brotli scratch buffers and the zstd compressor moved out of `thread_local!` storage and into a `CompressMeta` value that rayon creates once per worker via `map_init` and threads through the batch by `&mut`. The state is reused across the files a worker handles exactly as before, but no thread-local storage is involved: the brotli scratch is a plain inline array again rather than a heap `Vec` working around glibc's static TLS surplus in a `dlopen`ed `cdylib`, and neither buffer depends on the lifetime of the thread that first touched it.
- zstd is built with `default-features = false`, dropping the legacy-format decoder (v0.1–0.7), the array APIs and the dictionary builder — none of which this plugin calls — from the compiled C, and the `wasm32-wasi` build adds `no_asm` so it stops trying to assemble the x86 decompression routines.

