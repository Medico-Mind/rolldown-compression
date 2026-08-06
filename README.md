# @medicomind/rolldown-compression

[![CI](https://github.com/Medico-Mind/rolldown-compression/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Medico-Mind/rolldown-compression/actions/workflows/ci.yml)
[![npm version](https://img.shields.io/npm/v/%40medicomind%2Frolldown-compression?logo=npm)](https://www.npmjs.com/package/@medicomind/rolldown-compression)
[![npm downloads](https://img.shields.io/npm/dm/%40medicomind%2Frolldown-compression)](https://www.npmjs.com/package/@medicomind/rolldown-compression)
[![node](https://img.shields.io/node/v/%40medicomind%2Frolldown-compression)](https://www.npmjs.com/package/@medicomind/rolldown-compression)
[![license](https://img.shields.io/npm/l/%40medicomind%2Frolldown-compression)](./LICENSE)

Fast, native compression plugin for [Rolldown](https://rolldown.rs) and [Vite 8+](#usage-with-vite): compresses emitted assets with **gzip**, **brotli** and **zstd** at build time. The compression core is written in Rust (napi-rs + rayon) — one batched FFI call per build, fanned out across all CPU cores, without ever blocking the JS event loop.

**~5x faster builds in a real project**: switching a production app from `node:zlib`-based (node v26.4.0) compression to this plugin cut total build time from 4:21 to 53s (343% → 1378% CPU utilization) — see [real-world results](#real-world-results).

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
| `brotli`  | `sectionSize` | ≥ 1 (bytes) | `2^(windowBits + 1)` B |
| `zstd`    | `level`       | 1–22        | 19      |

```ts
defineAlgorithm('gzip', { level: 9 })
defineAlgorithm('brotli', { quality: 7, windowBits: 22 })
defineAlgorithm('zstd', { level: 12 })
```

`sectionSize` is the target number of bytes each brotli worker thread compresses when a large input is split across the native worker pool; inputs at least twice `sectionSize` take the multithreaded path. It defaults to two windows (`2^(windowBits + 1)` bytes) — 8 MiB and multithreading from 16 MiB at the default window — because sections much smaller than the window lose too many cross-section matches. Smaller sections finish large files faster at a slight cost in compression ratio.

An input is cut into one section per full `sectionSize`, up to one section per worker thread — more sections than there are threads only queue behind each other. A file large enough to hit that cap is therefore split according to the pool size (the machine's core count, or [`concurrency`](#options) when set), so its compressed bytes can differ between machines; smaller inputs are split by length alone and compress identically everywhere. On 100 MiB of real JS at quality 11 with 18 workers, one section per worker compresses 3.1x faster than the previous four-section limit for 0.1% more output.

**`windowBits` is the main speed/ratio dial.** It sets the brotli sliding window (`2^windowBits` bytes) — how far back a match can be found — and, through the `sectionSize` default above, how large a file has to be before it is split across threads. **Lowering it makes compression faster in two ways at once and costs compression ratio**: a smaller window is cheaper to search, *and* it drops the split threshold to `2^(windowBits + 2)` bytes, so big bundles are cut into sections sooner and finish on more cores (`windowBits: 20` splits from 4 MiB instead of 16 MiB). On the [benchmark](#synthetic-benchmark) fixtures, `windowBits: 20` takes brotli quality 11 from 10.08s to 6.48s. Raising it does the reverse: better ratio, less parallelism, slower builds.

The size penalty depends entirely on how far apart your content's repetitions are: it is real on production bundles (repeated vendor copies, inlined assets, source maps), but on those synthetic fixtures — whose matches are nearly all short-range — it stays within 0.1%, and there `windowBits: 20` even came out marginally *smaller*. **Measure your own output before shipping a lower window** — decoders also allocate the window the stream declares, and above 24 bits brotli needs the large-window extension that browsers do not implement, hence the 10–24 range. The default `22` matches `BROTLI_DEFAULT_WINDOW`, i.e. what `node:zlib` and the `brotli` CLI emit.

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

Switching a production app's build from `node:zlib`-based compression to this plugin (same algorithms and levels), on an Apple M5 Pro (18 cores), Node 26:

```
before: npm run build  890.47s user 4.43s system 343% cpu 4:20.62 total
after:  npm run build  724.15s user 6.24s system 1378% cpu 52.983 total
```

**4.92x faster wall clock.** Compression stops being serialized behind the libuv thread pool (default `UV_THREADPOOL_SIZE=4`) and runs on all cores instead — CPU utilization jumps from 343% to 1378%. Total CPU time also drops (895s → 730s), so the win is not purely parallelism: the native gzip/zstd backends do less work per byte than node's bundled zlib.

### Synthetic benchmark

`npm run bench` (or `node benchmark/index.mjs --quick`) generates a dist-shaped fixture set — 202 files / ~85 MB with a long-tail size distribution, including two monolithic >16 MiB bundles that exercise the multithreaded brotli path — and compresses it with the native core vs `node:zlib` driven at full parallelism via `Promise.all` (the reference plugin's best case). Both sides always use the same levels.

Results on an Apple M5 Pro (18 cores), Node 26, default `UV_THREADPOOL_SIZE`:

#### With PGO
| scenario                          | output   | native (rust) | node:zlib | speedup |
|-----------------------------------|----------|---------------|-----------|---------|
| gzip+brotli (ref. defaults: 9/11) | 15.07 MB | 9.72s         | 23.66s    | 2.43x   |
| gzip (level 9)                    | 9.62 MB  | 0.18s         | 0.53s     | 2.91x   |
| gzip (level 6)                    | 9.86 MB  | 0.11s         | 0.24s     | 2.07x   |
| brotli (quality 11)               | 5.45 MB  | 10.17s        | 23.50s    | 2.31x   |
| brotli (quality 6)                | 9.88 MB  | 0.15s         | 0.22s     | 1.42x   |
| zstd (level 19)                   | 5.54 MB  | 4.90s         | 8.03s     | 1.64x   |

#### Without PGO
| scenario                          | output   | native (rust) | node:zlib | speedup |
|-----------------------------------|----------|---------------|-----------|---------|
| gzip+brotli (ref. defaults: 9/11) | 15.07 MB | 10.29s        | 23.51s    | 2.29x   |
| gzip (level 9)                    | 9.62 MB  | 0.16s         | 0.53s     | 3.23x   |
| gzip (level 6)                    | 9.86 MB  | 0.12s         | 0.24s     | 1.99x   |
| brotli (quality 11)               | 5.45 MB  | 9.82s         | 23.66s    | 2.41x   |
| brotli (quality 6)                | 9.88 MB  | 0.15s         | 0.22s     | 1.41x   |
| zstd (level 19)                   | 5.54 MB  | 4.93s         | 8.02s     | 1.63x   |

Reading these numbers honestly:

- **gzip** and **zstd** are faster per core ([zlib-rs](https://github.com/trifectatechfoundation/zlib-rs), ~2.4x faster per core than node's bundled zlib in our measurements; libzstd) *and* use every core, while `node:zlib` is capped at `UV_THREADPOOL_SIZE` (default 4) threads. The two >16 MiB bundles temper the headline numbers: neither algorithm has a sectioned mode, so each giant file occupies a single thread on both sides and that tail runs at the per-core ratio rather than the thread-count ratio.
- **brotli at quality 11** is the bound on the combined number: the Rust `brotli` crate is at per-core parity with node's C brotli (we measured a 1.01 single-thread ratio), so the speedup is parallelism — every core against `UV_THREADPOOL_SIZE` threads across the many small files, plus the sectioned worker pool (2 x 8 MiB sections) on the >16 MiB bundles that `node:zlib` has to compress one thread per file.
- **The brotli rows are slower than the previous release's tables, by design.** `sectionSize` used to default to one window (4 MiB at `windowBits: 22`), splitting a >16 MiB input into up to 4 sections; it now defaults to two windows (8 MiB), so the fixture set's two ~20 MiB bundles are cut into 2 sections instead of 4 and their tail runs on half as many cores. Brotli quality 11 went from ~6.9s to ~10.2s and the combined scenario from 3.34x to 2.43x. Holding the *current* binding fixed and passing only `sectionSize: 4 * 1024 * 1024` brings quality 11 back to 7.29s (vs 10.08s at the default), so the regression is the new default rather than the rayon worker-pool rewrite that landed alongside it. Pass `sectionSize` explicitly if you want the old wall clock.
- **The ratio this buys is not visible on these fixtures.** The larger default exists because sections much smaller than the window lose cross-section matches, but at quality 11 the same 85 MB comes out **5.450 MB with 8 MiB sections and 5.445 MB with 4 MiB ones** — 0.09%, and in the wrong direction, i.e. block-splitting noise rather than a real gain. The generated fixtures repeat a small vocabulary line by line, so nearly every match is short-range and even a 4 MiB window already finds it. Expect the trade to favor the larger default on real bundles with long-range redundancy (repeated vendor copies, inlined assets, source maps) — and measure your own output before trading wall clock for it.
- The speedup grows with core count and shrinks if you raise `UV_THREADPOOL_SIZE` for the JS side — the benchmark prints both so runs are comparable. The 10-core M1 Pro these tables previously covered landed around 2.4x on the combined scenario, and the 18-core M5 Pro reaches 2.3–2.4x at the current section default (it reached 3.3–3.6x at the old 4 MiB one) against the same 4-thread JS side.
- The two tables above are single runs of each binding, so the small differences *between* them are run-to-run noise, not a PGO effect — see [PGO / BOLT builds](#pgo--bolt-builds) for an interleaved median comparison of the same two binaries.

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

Expect modest gains at best: the baseline already ships fat LTO with `codegen-units = 1`, so there is little left for PGO to find. On an M5 Pro the interleaved medians come out at parity (0.96x–1.03x across the compression-heavy scenarios); an earlier M1 Pro run measured ~1.1x on brotli quality 11. Treat single-digit-percent deltas in either direction — and every sub-second scenario — as measurement noise rather than a real speedup or regression.

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
