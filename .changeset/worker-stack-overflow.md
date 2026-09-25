---
"@medicomind/rolldown-compression": patch
---

Fix intermittent `illegal hardware instruction` crashes while compressing: a stack overflow on the rayon workers. Single-section brotli inputs run inline again instead of joining a parallel iterator, and the worker pool is always built with 16 MiB stacks.
