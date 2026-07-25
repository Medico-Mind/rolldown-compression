---
"@medicomind/rolldown-compression": patch
---

Default brotli `sectionSize` to one window (`2^windowBits` bytes) instead of a fixed 4 MiB, so a custom `windowBits` gets a matching section size. Unchanged at the default window of 22, where both are 4 MiB.
