---
"@medicomind/rolldown-compression": minor
---

Follow the Rolldown plugin conventions and harden asset handling.

- The plugin is now named `rolldown-plugin-compression` (was `rolldown-compression`), as the [plugin conventions](https://rolldown.rs/apis/plugin-api#conventions) ask for. It also reports `version` and `meta.packageName`, and exposes an `api` (`algorithms`, `extensions`, `emittedFileNames()`) for inter-plugin communication.
- `deleteOriginalAssets` no longer removes an asset that ended up without a compressed variant — a file skipped by `skipIfLargerOrEqual` used to be deleted with nothing to replace it. Removing a chunk now logs a warning once, since only the compressed name is left on disk.
- Artifact names resolved from `filename` are validated: names that escape the output directory, are absolute, collide with another artifact of the same build, or would overwrite a file the build already owns now fail the build instead of silently clobbering.
- In stream mode `deleteOriginalAssets` now only unlinks files the build actually wrote — those the bundle declares, or those written while the output was being generated. A leftover from an earlier build in an output directory that was not emptied still gets compressed, but is no longer deleted.
- Per-output state is reset in `renderStart`, so with multiple outputs one output's artifacts no longer shadow another output's sources, and watch-mode rebuilds no longer accumulate it.
- Per-file detail (`skipped …`) moved from `this.info` to `this.debug`; the build summary stays on `this.info`.
- Assets no longer cross into the native module through a duplicated `Buffer`, and stream mode reads each batch's files concurrently instead of one at a time.
