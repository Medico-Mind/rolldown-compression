# @medicomind/rolldown-compression

Fast, native compression plugin for [Rolldown](https://rolldown.rs): compresses emitted assets with **gzip**, **brotli** and **zstd** at build time. The compression core is written in Rust (napi-rs + rayon) — one batched FFI call per build, fanned out across all CPU cores, without ever blocking the JS event loop.

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
  logLevel: 'info',
})
```

## Options

| option | type | default | description |
| --- | --- | --- | --- |
| `include` | `string \| RegExp \| Array<string \| RegExp>` | `/\.(html\|xml\|css\|json\|js\|mjs\|svg\|yaml\|yml\|toml\|txt\|wasm)$/` | Files to compress. Strings are [picomatch](https://github.com/micromatch/picomatch) globs, matched against bundle-relative file names ([`@rollup/pluginutils` `createFilter`](https://github.com/rollup/plugins/tree/master/packages/pluginutils#createfilter) semantics). |
| `exclude` | `string \| RegExp \| Array<string \| RegExp>` | — | Files to skip. **Wins over `include`.** |
| `threshold` | `number` | `0` | Minimum original size in bytes for a file to be compressed. |
| `algorithms` | `Array<AlgorithmName \| DefineAlgorithmResult>` | `['gzip', 'brotli']` | Algorithms to run. Aliases `gz`, `br`, `brotliCompress`, `zstandard` normalize to `gzip` / `brotli` / `zstd`. |
| `filename` | `string \| (fileName, algorithm) => string` | `'[path][base]' + ext` | Name of the emitted artifact. Tokens: `[path]` (directory incl. trailing `/`), `[base]`, `[name]`, `[ext]` (with dot), `[hash]` (8-char sha256 of the content). Default extensions: `.gz`, `.br`, `.zst`. |
| `deleteOriginalAssets` | `boolean` | `false` | Remove the original from the bundle after all algorithms processed it. Errors if `filename` resolves to the source name. |
| `skipIfLargerOrEqual` | `boolean` | `true` | Don't emit artifacts whose compressed size is `>=` the original. |
| `concurrency` | `number` | `0` | Native worker threads. `0` = number of logical CPUs. |
| `logLevel` | `'silent' \| 'error' \| 'warn' \| 'info'` | `'info'` | Plugin log verbosity; `info` prints a per-algorithm summary at build end. |
| `enableInWatchMode` | `boolean` | `false` | The plugin is a no-op in watch/dev mode unless enabled (see [watch mode](#watch--dev-mode)). |

All options are validated when `compression()` is called — invalid levels (e.g. brotli quality 12), unknown algorithm names or malformed filters throw immediately, not mid-build.

### Per-algorithm options

| algorithm | option | range | default |
| --- | --- | --- | --- |
| `gzip` | `level` | 0–9 | 6 |
| `brotli` | `quality` | 0–11 | 11 |
| `brotli` | `windowBits` | 10–24 | 22 |
| `zstd` | `level` | 1–22 | 19 |

```ts
defineAlgorithm('gzip', { level: 9 })
defineAlgorithm('brotli', { quality: 7, windowBits: 22 })
defineAlgorithm('zstd', { level: 12 })
```

## How it works

- The plugin hooks **`generateBundle`**, while all chunks and assets are still in memory — no filesystem round-trip. Eligible files (filter + threshold) are sent to the native module as **one batched FFI call per build**; results are emitted with `emitFile`.
- Compression runs on a rayon thread pool inside the native module (`AsyncTask`), parallel across files *and* algorithms, scheduled most-expensive-first so one large brotli file can't stretch the batch tail. The JS event loop keeps ticking throughout (covered by a test).
- Buffers cross the FFI boundary without base64/string round-trips, and the compression working set lives in native memory — a 500 MB asset does not pressure the JS heap.
- A failing task never aborts the batch: per-task errors are aggregated and fail the build with one message.
- Already-compressed artifacts (`.gz`, `.br`, `.zst` — ours or pre-existing) are never re-compressed, so chaining plugin instances can't produce `app.js.gz.br`.

Limitation: assets written to disk by other plugins in `writeBundle`/`closeBundle` (i.e. after `generateBundle`) are not seen by this plugin. This matches how `vite-plugin-compression2` handles the in-bundle pass; a `writeBundle` fallback was deliberately left out to keep the pipeline zero-copy and single-pass. File an issue if you have a concrete case.

### Watch / dev mode

The plugin declares `apply: 'build'` (honored by Vite-style hosts such as `rolldown-vite`) **and** checks `this.meta.watchMode` at `generateBundle` time, making it a no-op under `rolldown --watch`. Set `enableInWatchMode: true` to compress in watch builds anyway.

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

`npm run bench` (or `node benchmark/index.mjs --quick`) generates a dist-shaped fixture set — 200 files / ~48 MB with a long-tail size distribution — and compresses it with the native core vs `node:zlib` driven at full parallelism via `Promise.all` (the reference plugin's best case). Both sides always use the same levels.

Results on an Apple M1 Pro (10 cores), Node 26, default `UV_THREADPOOL_SIZE`:

| scenario | output | native (rust) | node:zlib | speedup |
| --- | --- | --- | --- | --- |
| gzip+brotli (ref. defaults: 9/11) | 8.71 MB | 8.82s | 15.19s | **1.72x** |
| gzip (level 9) | 5.51 MB | 0.10s | 0.33s | **3.48x** |
| gzip (level 6) | 5.64 MB | 0.07s | 0.16s | **2.38x** |
| brotli (quality 11) | 3.21 MB | 8.16s | 14.96s | **1.83x** |
| zstd (level 19) | 3.28 MB | 2.19s | 6.91s | **3.15x** |

Reading these numbers honestly:

- **gzip** and **zstd** beat the ≥3x target: the Rust encoders ([zlib-rs](https://github.com/trifectatechfoundation/zlib-rs), ~2.4x faster per core than node's bundled zlib in our measurements; libzstd) are faster per core *and* use every core, while `node:zlib` is capped at `UV_THREADPOOL_SIZE` (default 4) threads.
- **brotli at quality 11** is the bound on the combined number: the Rust `brotli` crate is at per-core parity with node's C brotli (we measured a 1.01 single-thread ratio), so the achievable speedup is roughly `cores / UV_THREADPOOL_SIZE` — ~2x on 8 equal cores, more on bigger machines. No implementation can honestly do better without changing the algorithm or its level.
- The speedup grows with core count and shrinks if you raise `UV_THREADPOOL_SIZE` for the JS side — the benchmark prints both so runs are comparable.

## Platform support

Prebuilt binaries are published for:

| platform | triple |
| --- | --- |
| macOS arm64 | `aarch64-apple-darwin` |
| macOS x64 | `x86_64-apple-darwin` |
| Linux x64 (glibc) | `x86_64-unknown-linux-gnu` |
| Linux arm64 (glibc) | `aarch64-unknown-linux-gnu` |
| Linux x64 (musl) | `x86_64-unknown-linux-musl` |
| Windows x64 | `x86_64-pc-windows-msvc` |

Node.js >= 18.

## Differences from vite-plugin-compression2

- **Native speed**: compression runs in Rust on all cores, one FFI batch per build, instead of `node:zlib` calls through the libuv thread pool.
- **No custom JS algorithms**: `algorithms` accepts only the built-in `gzip` / `brotli` / `zstd` (function-form algorithms can't cross the FFI boundary). `defineAlgorithm` returns an opaque object, not a `[name, options]` tuple — treat it as such.
- **No tarball plugin**: out of scope.
- **gzip default level is 6** (zlib default), not 9 — measurably faster for a ~1% size difference. Pass `defineAlgorithm('gzip', { level: 9 })` to match the reference.
- **zstd everywhere**: zstd is compiled in, with no dependency on the Node runtime's zstd support (node >= 22.15).
- Extra options: `concurrency` (native thread cap) and `enableInWatchMode`.

## Implementation decisions

- **Rolldown target**: developed and tested against `rolldown@1.1.x` (peer range `^1.0.0`), using the Rollup-compatible `generateBundle`/`emitFile` plugin API.
- **gzip backend**: `flate2` with the pure-Rust `zlib-rs` backend — as fast as or faster than zlib-ng in our runs, with no cmake/C toolchain requirement for contributors.
- **Publishing**: public npm (`--access public`), versioned with [changesets](https://github.com/changesets/changesets). PRs include a changeset (`npx changeset`); the Version workflow keeps a `chore: release` PR up to date, and merging it tags the release and runs the full napi build matrix before `napi prepublish` + `npm publish`. Run the Release workflow via `workflow_dispatch` for a dry-run that builds all platform artifacts without publishing.

## Development

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
