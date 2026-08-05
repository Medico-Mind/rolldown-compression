---
"@medicomind/rolldown-compression": minor
---

Build the x86_64 binaries for the x86-64-v2 baseline.

- The prebuilt `x86_64` bindings (linux-gnu, linux-musl, darwin and windows-msvc) are now compiled with `-C target-cpu=x86-64-v2`, and the C dependencies on the linux and darwin targets with the matching `-march=x86-64-v2`. That lets the compressors use SSE3 through SSE4.2, POPCNT and CMPXCHG16B directly instead of the 2003-era baseline rustc and cc default to.
- The requirement that follows: the `x86_64` binaries need a CPU from Intel Nehalem (2008) or AMD Bulldozer (2011) onwards. Every x86_64 machine and cloud instance still in service clears that bar, but a build host older than those, or a VM pinned to an emulated pre-Nehalem CPU model, will now fault on load instead of running. `aarch64` and `wasm32-wasi` builds are untouched.

