---
"@medicomind/rolldown-compression": patch
---

Pass each source file to Rust once per batch and share its buffer across compression algorithms, with algorithm settings sent separately.
