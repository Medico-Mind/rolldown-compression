---
"@medicomind/rolldown-compression": patch
---

Reuse Brotli encoder allocations through a process-wide cache to reduce allocation count and volume across concurrent compression jobs.
