---
"@medicomind/rolldown-compression": patch
---

Fix the published `wasm32-wasi` package manifest, which had drifted behind the napi-rs CLI: it no longer declares `cpu: ["wasm32"]` (the WASI binding runs on any host architecture), now declares `type: module` for its loaders, ships and points `types` at `rolldown-compression.wasi.d.cts`, and pins the `@emnapi/core` / `@emnapi/runtime` versions the binding is actually built against.
