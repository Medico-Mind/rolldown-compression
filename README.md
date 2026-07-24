# @medicomind/rolldown-compression

[![CI](https://github.com/Medico-Mind/rolldown-compression/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Medico-Mind/rolldown-compression/actions/workflows/ci.yml)
[![npm version](https://img.shields.io/npm/v/%40medicomind%2Frolldown-compression?logo=npm)](https://www.npmjs.com/package/@medicomind/rolldown-compression)
[![npm downloads](https://img.shields.io/npm/dm/%40medicomind%2Frolldown-compression)](https://www.npmjs.com/package/@medicomind/rolldown-compression)
[![node](https://img.shields.io/node/v/%40medicomind%2Frolldown-compression)](https://www.npmjs.com/package/@medicomind/rolldown-compression)
[![license](https://img.shields.io/npm/l/%40medicomind%2Frolldown-compression)](./LICENSE)

Fast, native compression plugin for [Rolldown](https://rolldown.rs) and [Vite 8+](#usage-with-vite): compresses emitted assets with **gzip**, **brotli** and **zstd** at build time. The compression core is written in Rust (napi-rs + rayon) — one batched FFI call per build, fanned out across all CPU cores, without ever blocking the JS event loop.

**3x faster builds in a real project**: switching a production app from `node:zlib`-based (node v26.4.0) compression to this plugin cut total build time from 3:50 to 1:13 (740% → 688% CPU utilization) — see [real-world results](#real-world-results).

API ergonomics mirror [`vite-plugin-compression2`](https://github.com/nonzzz/vite-plugin-compression); see [differences](#differences-from-vite-plugin-compression2).

## Install

```sh
npm install -D @medicomind/rolldown-compression
```

Prebuilt binaries are installed automatically — no Rust toolchain needed (see [platform support](#platform-support)).

## Quick start

```ts
// rolldown.config.ts
import { defineConfig } from 'rolldown'
import { compression } from '@medicomind/rolldown-compression'

export default defineConfig({
  input: 'src/main.ts',
  plugins: [
    // gzip + brotli with defaults
    compression(),
  ],
})
```

Full configuration:

```ts
import { compression, defineAlgorithm } from '@medicomind/rolldown-compression'

compression({
  include: [/\.(js|mjs|css|html|svg|json|wasm)$/],
  exclude: [/\.(png|jpe?g|webp|woff2?)$/],
  threshold: 1024,
  algorithms: [
    'gzip', // string shorthand with default level
    defineAlgorithm('brotli', { quality: 11 }),
    defineAlgorithm('zstd', { level: 19 }),
  ],
  filename: '[path][base].gz', // or (fileName, algorithm) => string
  deleteOriginalAssets: false,
  skipIfLargerOrEqual: true,
  concurrency: 0, // 0 = number of logical CPUs
  chunkSize: 0, // 0 = compress everything in one batch
  stream: false, // true = on-demand disk-based compression in writeBundle
  logLevel: 'info',
})
```

## Usage with Vite

[Vite 8+](https://vite.dev/blog/announcing-vite8) uses Rolldown as its bundler, so the plugin works there out of the box:

```ts
// vite.config.ts
import { defineConfig } from 'vite'
import { compression } from '@medicomind/rolldown-compression'

export default defineConfig({
  plugins: [compression()],
})
```

- The plugin declares `apply: 'build'`, so it only runs for `vite build` — the dev server is untouched.
- On Vite 6/7 use the [`rolldown-vite`](https://vite.dev/guide/rolldown) package (aliased as `vite`) to get the Rolldown-based build.
- All [options](#options) are the same as with plain Rolldown, and mirror [`vite-plugin-compression2`](https://github.com/nonzzz/vite-plugin-compression) — for most projects it's a drop-in replacement (see [differences](#differences-from-vite-plugin-compression2)).

## Options

| option                 | type                                            | default                                                                 | description                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
|------------------------|-------------------------------------------------|-------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `include`              | `string \| RegExp \| Array<string \| RegExp>`   | `/\.(html\|xml\|css\|json\|js\|mjs\|svg\|yaml\|yml\|toml\|txt\|wasm)$/` | Files to compress. Strings are [picomatch](https://github.com/micromatch/picomatch) globs, matched against bundle-relative file names ([`@rollup/pluginutils` `createFilter`](https://github.com/rollup/plugins/tree/master/packages/pluginutils#createfilter) semantics).                                                                                                                                                                                             |
| `exclude`              | `string \| RegExp \| Array<string \| RegExp>`   | —                                                                       | Files to skip. **Wins over `include`.**                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `threshold`            | `number`                                        | `0`                                                                     | Minimum original size in bytes for a file to be compressed.                                                                                                                                                                                                                                                                                                                                                                                                            |
| `algorithms`           | `Array<AlgorithmName \| DefineAlgorithmResult>` | `['gzip', 'brotli']`                                                    | Algorithms to run. Aliases `gz`, `br`, `brotliCompress`, `zstandard` normalize to `gzip` / `brotli` / `zstd`.                                                                                                                                                                                                                                                                                                                                                          |
| `filename`             | `string \| (fileName, algorithm) => string`     | `'[path][base]' + ext`                                                  | Name of the emitted artifact. Tokens: `[path]` (directory incl. trailing `/`), `[base]`, `[name]`, `[ext]` (with dot), `[hash]` (8-char sha256 of the content). Default extensions: `.gz`, `.br`, `.zst`.                                                                                                                                                                                                                                                              |
| `deleteOriginalAssets` | `boolean`                                       | `false`                                                                 | Remove the original from the bundle after all algorithms processed it. Errors if `filename` resolves to the source name.                                                                                                                                                                                                                                                                                                                                               |
| `skipIfLargerOrEqual`  | `boolean`                                       | `true`                                                                  | Don't emit artifacts whose compressed size is `>=` the original.                                                                                                                                                                                                                                                                                                                                                                                                       |
| `concurrency`          | `number`                                        | `0`                                                                     | Native worker threads. `0` = number of logical CPUs.                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `chunkSize`            | `number`                                        | `0`                                                                     | Max source bytes buffered per native compression batch. `0` = one batch for the whole build. A positive value (e.g. `64 * 1024 * 1024`) caps the plugin's peak memory overhead at roughly one batch of source copies plus its compressed outputs; a single file larger than `chunkSize` still forms its own batch. The bundler keeps the original bundle in memory regardless. In [stream mode](#stream-mode) `0` falls back to a 4 MB batch instead of one big batch. |
| `stream`               | `boolean`                                       | `false`                                                                 | Compress from disk in `writeBundle` (order `post`) instead of in memory in `generateBundle`: files are read on demand in bounded batches (`chunkSize` bytes, default 4 MB), and assets written to disk by other plugins' `writeBundle` hooks are compressed too (see [stream mode](#stream-mode)).                                                                                                                                                                     |
| `logLevel`             | `'silent' \| 'error' \| 'warn' \| 'info'`       | `'info'`                                                                | Plugin log verbosity; `info` prints a per-algorithm summary at build end.                                                                                                                                                                                                                                                                                                                                                                                              |
| `enableInWatchMode`    | `boolean`                                       | `false`                                                                 | The plugin is a no-op in watch/dev mode unless enabled (see [watch mode](#watch--dev-mode)).                                                                                                                                                                                                                                                                                                                                                                           |

All options are validated when `compression()` is called — invalid levels (e.g. brotli quality 12), unknown algorithm names or malformed filters throw immediately, not mid-build.

### Per-algorithm options

| algorithm | option        | range       | default |
|-----------|---------------|-------------|---------|
| `gzip`    | `level`       | 0–9         | 6       |
| `brotli`  | `quality`     | 0–11        | 11      |
| `brotli`  | `windowBits`  | 10–24       | 22      |
| `brotli`  | `sectionSize` | ≥ 1 (bytes) | 4 MiB   |
| `zstd`    | `level`       | 1–22        | 19      |

```ts
defineAlgorithm('gzip', { level: 9 })
defineAlgorithm('brotli', { quality: 7, windowBits: 22 })
defineAlgorithm('zstd', { level: 12 })
```

`sectionSize` is the target number of bytes each brotli worker thread compresses when a large input is split across the native worker pool; inputs at least four times `sectionSize` take the multithreaded path (16 MiB at the default). Smaller sections finish large files faster at a slight cost in compression ratio — sections much smaller than the window (`2^windowBits` bytes) lose too many cross-section matches.

## How it works

- The plugin hooks **`generateBundle`**, while all chunks and assets are still in memory — no filesystem round-trip. Eligible files (filter + threshold) are sent to the native module as **one batched FFI call per build**; results are emitted with `emitFile`.
- Compression runs on a rayon thread pool inside the native module (`AsyncTask`), parallel across files *and* algorithms, scheduled most-expensive-first so one large brotli file can't stretch the batch tail. The JS event loop keeps ticking throughout (covered by a test).
- Buffers cross the FFI boundary without base64/string round-trips, and the compression working set lives in native memory — a 500 MB asset does not pressure the JS heap.
- A failing task never aborts the batch: per-task errors are aggregated and fail the build with one message.
- Already-compressed artifacts (`.gz`, `.br`, `.zst` — ours or pre-existing) are never re-compressed, so chaining plugin instances can't produce `app.js.gz.br`.

Limitation of the default mode: assets written to disk by other plugins in `writeBundle`/`closeBundle` (i.e. after `generateBundle`) are not seen. This matches how `vite-plugin-compression2` handles the in-bundle pass. Set `stream: true` to remove it.

### Stream mode

With `stream: true` the plugin skips the in-memory `generateBundle` pass entirely and instead runs at the end of `writeBundle` (hook order `post`), after the bundle — and any extra assets other plugins wrote in their own `writeBundle` hooks — is on disk:

```ts
compression({
  stream: true,
  chunkSize: 64 * 1024 * 1024, // optional: batch by source bytes instead of by file count
})
```

- The output directory is scanned recursively and matching files are read **on demand**, never all at once: a batch is flushed to the native module whenever it reaches `chunkSize` source bytes, defaulting to **4 MB** when `chunkSize` is `0`. Peak memory overhead is one batch of sources plus its compressed outputs, regardless of build size.
- Compressed artifacts are written straight to the output directory (`emitFile` is not available after write); `deleteOriginalAssets` unlinks the originals from disk.
- The same filter, threshold and re-compression guards apply, and everything already in the output directory that matches them is compressed — including files produced by other plugins after `generateBundle`, the default mode's limitation. Assets written in `closeBundle` (after all `writeBundle` hooks) are still not seen.
- Trade-off: files the bundler already had in memory are re-read from disk, and per-batch FFI calls replace the single big batch — for small builds the default in-memory mode is faster.

### Watch / dev mode

The plugin declares `apply: 'build'` (honored by Vite / `rolldown-vite`) **and** checks `this.meta.watchMode` at `generateBundle` time, making it a no-op under `rolldown --watch`. Set `enableInWatchMode: true` to compress in watch builds anyway.

## Serving pre-compressed assets

nginx ([gzip_static](https://nginx.org/en/docs/http/ngx_http_gzip_static_module.html) / [brotli_static](https://github.com/google/ngx_brotli) / [zstd_static](https://github.com/tokers/zstd-nginx-module)):

```nginx
location / {
  gzip_static on;     # serves foo.js.gz when the client accepts gzip
  brotli_static on;   # requires ngx_brotli
  zstd_static on;     # requires zstd-nginx-module
}
```

Caddy:

```caddyfile
example.com {
  root * /srv/dist
  file_server {
    precompressed zstd br gzip
  }
}
```

## Benchmark

### Real-world results

Switching a production app's build from `node:zlib`-based compression to this plugin (same algorithms and levels):

```
before: npm run build  675.82s user 4.63s system 294% cpu 3:50.92 total
after:  npm run build  537.24s user 5.59s system 740% cpu 1:13.29 total
```

**4.03x faster wall clock.** Compression stops being serialized behind the libuv thread pool (default `UV_THREADPOOL_SIZE=4`) and runs on all cores instead — CPU utilization jumps from 235% to 784%.

### Synthetic benchmark

`npm run bench` (or `node benchmark/index.mjs --quick`) generates a dist-shaped fixture set — 202 files / ~85 MB with a long-tail size distribution, including two monolithic >16 MiB bundles that exercise the multithreaded brotli path — and compresses it with the native core vs `node:zlib` driven at full parallelism via `Promise.all` (the reference plugin's best case). Both sides always use the same levels.

Results on an Apple M1 Pro (10 cores), Node 26, default `UV_THREADPOOL_SIZE`:

#### With PGO
| scenario                          | output   | native (rust) | node:zlib | speedup |
|-----------------------------------|----------|---------------|-----------|---------|
| gzip+brotli (ref. defaults: 9/11) | 15.06 MB | 17.61s        | 36.74s    | 2.09x   |
| gzip (level 9)                    | 9.62 MB  | 0.31s         | 0.80s     | 2.60x   |
| gzip (level 6)                    | 9.86 MB  | 0.18s         | 0.37s     | 2.09x   |
| brotli (quality 11)               | 5.45 MB  | 17.43s        | 36.65s    | 2.10x   |
| brotli (quality 6)                | 9.88 MB  | 0.28s         | 0.40s     | 1.40x   |
| zstd (level 19)                   | 5.54 MB  | 8.28s         | 14.35s    | 1.73x   |

#### Without PGO
| scenario                          | output   | native (rust) | node:zlib | speedup |
|-----------------------------------|----------|---------------|-----------|---------|
| gzip+brotli (ref. defaults: 9/11) | 15.06 MB | 18.34s        | 36.62s    | 2.00x   |
| gzip (level 9)                    | 9.62 MB  | 0.28s         | 0.80s     | 2.85x   |
| gzip (level 6)                    | 9.86 MB  | 0.19s         | 0.37s     | 1.93x   |
| brotli (quality 11)               | 5.45 MB  | 19.67s        | 36.50s    | 1.86x   |
| brotli (quality 6)                | 9.88 MB  | 0.31s         | 0.40s     | 1.27x   |
| zstd (level 19)                   | 5.54 MB  | 8.35s         | 14.98s    | 1.79x   |

Reading these numbers honestly:

- **gzip** and **zstd** are faster per core ([zlib-rs](https://github.com/trifectatechfoundation/zlib-rs), ~2.4x faster per core than node's bundled zlib in our measurements; libzstd) *and* use every core, while `node:zlib` is capped at `UV_THREADPOOL_SIZE` (default 4) threads. The two >16 MiB bundles temper the headline numbers: neither algorithm has a sectioned mode, so each giant file occupies a single thread on both sides and that tail runs at the per-core ratio rather than the thread-count ratio.
- **brotli at quality 11** is the bound on the combined number: the Rust `brotli` crate is at per-core parity with node's C brotli (we measured a 1.01 single-thread ratio), so the speedup is parallelism — every core against `UV_THREADPOOL_SIZE` threads across the many small files, plus the sectioned worker pool (4 x 4 MiB sections) on the >16 MiB bundles that `node:zlib` has to compress one thread per file.
- The speedup grows with core count and shrinks if you raise `UV_THREADPOOL_SIZE` for the JS side — the benchmark prints both so runs are comparable.

### PGO / BOLT builds

`npm run build:pgo` (scripts/pgo/build.mjs) produces a profile-guided release build:

1. baseline release build → `target/pgo/baseline.node`
2. instrumented build (`-Cprofile-generate`)
3. training run over a static corpus (`scripts/pgo/corpus.mjs`: JS bundles, JSON, CSS, HTML, SVG sprites, source maps, base64 blobs, incompressible noise — every algorithm at fast/default/max levels)
4. `llvm-profdata merge` (uses the rustup `llvm-tools` component; `rustup component add llvm-tools` if missing)
5. optimized rebuild (`-Cprofile-use`) → `target/pgo/pgo.node`, also installed as the platform binding in the repo root
6. on Linux ELF targets with `llvm-bolt`/`merge-fdata` on PATH, a BOLT post-link pass (instrument → retrain → `-reorder-blocks=ext-tsp` layout optimization) → `target/pgo/bolt.node`. BOLT does not support Mach-O/PE, so this step is skipped on macOS and Windows.

`npm run bench:pgo` (or with `--quick`) then benchmarks baseline vs PGO(+BOLT) on the same dist-shaped fixtures as `npm run bench`, with interleaved iterations and median timings:

| scenario       | what it measures                                           |
|----------------|------------------------------------------------------------|
| baseline       | plain `--release` (fat LTO, `codegen-units = 1`)           |
| pgo / pgo+bolt | same flags plus `-Cprofile-use` (and BOLT layout on Linux) |

Expect modest gains: the baseline already ships fat LTO with `codegen-units = 1`, so PGO adds a few percent on the compression-heavy scenarios (~1.1x on the brotli quality 11 run on an M1 Pro) and is within noise on sub-second ones. Sub-second scenario deltas in the table are measurement noise, not regressions.

The release workflow builds every published binary with PGO. Cross-compiled targets run the training workload through an emulation layer — x64 Node under Rosetta 2 for `x86_64-apple-darwin`, an arm64 Node container under QEMU for `aarch64-unknown-linux-gnu`, and an Alpine container for musl — so each target trains on its own instrumented binding.

## Platform support

Prebuilt binaries are published for:

| platform            | triple                      |
|---------------------|-----------------------------|
| macOS arm64         | `aarch64-apple-darwin`      |
| macOS x64           | `x86_64-apple-darwin`       |
| Linux x64 (glibc)   | `x86_64-unknown-linux-gnu`  |
| Linux arm64 (glibc) | `aarch64-unknown-linux-gnu` |
| Linux x64 (musl)    | `x86_64-unknown-linux-musl` |
| Windows x64         | `x86_64-pc-windows-msvc`    |

Node.js >= 22.14.0 (since v2; v1.x supports Node.js >= 18).

### WebAssembly fallback

A [WASI build](https://napi.rs/docs/concepts/webassembly)
(`@medicomind/rolldown-compression-wasm32-wasi`) is published for platforms
without a prebuilt native binary. The loader falls back to it automatically
when no native binding can be loaded — expect several times slower compression
than native (~5x in a quick local benchmark).

Package managers skip optional dependencies whose `cpu` field doesn't match
the host, so on an unsupported platform the wasm package must be opted into:

- **npm**: `npm install --cpu wasm32 @medicomind/rolldown-compression-wasm32-wasi`
  (or add it as a regular `devDependency`).
- **yarn**: add `supportedArchitectures: { cpu: ["current", "wasm32"] }` to `.yarnrc.yml`.
- **pnpm**: add `supportedArchitectures: { cpu: ["current", "wasm32"] }` under `pnpm` in `package.json`.

## Differences from vite-plugin-compression2

- **Native speed**: compression runs in Rust on all cores, one FFI batch per build, instead of `node:zlib` calls through the libuv thread pool.
- **No custom JS algorithms**: `algorithms` accepts only the built-in `gzip` / `brotli` / `zstd` (function-form algorithms can't cross the FFI boundary). `defineAlgorithm` returns an opaque object, not a `[name, options]` tuple — treat it as such.
- **No tarball plugin**: out of scope.
- **gzip default level is 6** (zlib default), not 9 — measurably faster for a ~1% size difference. Pass `defineAlgorithm('gzip', { level: 9 })` to match the reference.
- **zstd everywhere**: zstd is compiled in, with no dependency on the Node runtime's zstd support (node >= 22.15).
- Extra options: `concurrency` (native thread cap), `chunkSize` (memory cap per compression batch), `stream` (on-demand disk-based compression in `writeBundle`) and `enableInWatchMode`.

## Implementation decisions

- **Rolldown target**: developed and tested against `rolldown@1.1.x` (peer range `^1.0.0`), using the Rollup-compatible `generateBundle`/`emitFile` plugin API.
- **gzip backend**: `flate2` with the pure-Rust `zlib-rs` backend — as fast as or faster than zlib-ng in our runs, with no cmake/C toolchain requirement for contributors.
- **Publishing**: public npm (`--access public`), versioned with [changesets](https://github.com/changesets/changesets). PRs include a changeset (`npx changeset`); the Version workflow keeps a `chore: release` PR up to date, and merging it tags the release and runs the full napi build matrix before `napi prepublish` + `npm publish`. Run the Release workflow via `workflow_dispatch` for a dry-run that builds all platform artifacts without publishing.

## Development

See [CONTRIBUTING.md](./CONTRIBUTING.md) for the full contributor guide (setup, tests, changesets, PR workflow).

```sh
npm install          # install JS deps
npm run build        # release native build + TS bundle
npm test             # vitest (unit + integration)
cargo test           # Rust core tests
npm run bench        # benchmark vs node:zlib
COMPRESSION_TEST_LARGE=1 npx vitest run __tests__/integration/large-file.test.ts  # 150 MB asset test
npx changeset        # add a changeset describing your change (required for releases)
```

## License

MIT
