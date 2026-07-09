---
"@medicomind/rolldown-compression": patch
---

Declare the platform binary packages in `optionalDependencies` so npm actually installs the native binding. 0.3.1 shipped without them, causing "Cannot find native binding" on import.
