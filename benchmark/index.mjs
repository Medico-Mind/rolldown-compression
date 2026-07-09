/**
 * Benchmark: native Rust batch compression vs the `node:zlib` approach used
 * by JS compression plugins such as vite-plugin-compression2.
 *
 * Both sides compress the exact same fixture set at the same levels. The JS
 * side fans out with Promise.all, which lets node:zlib use its libuv thread
 * pool (UV_THREADPOOL_SIZE, default 4) — i.e. this compares against the
 * reference plugin's best case, not a strawman serial loop.
 *
 * The fixture set models a real dist/ folder: ~200 files / ~50 MB with a
 * long-tail size distribution (many small route chunks, a few large vendor
 * bundles).
 *
 * Usage: node benchmark/index.mjs [--quick]
 */
import { createRequire } from 'node:module'
import { availableParallelism } from 'node:os'
import { promisify } from 'node:util'
import zlib from 'node:zlib'

const require = createRequire(import.meta.url)
const { compressBuffers } = require('../index.js')

const QUICK = process.argv.includes('--quick')
const SCALE = QUICK ? 0.2 : 1

const gzipAsync = promisify(zlib.gzip)
const brotliAsync = promisify(zlib.brotliCompress)
const zstdAsync = zlib.zstdCompress ? promisify(zlib.zstdCompress) : null

/** Deterministic, code-shaped fixture content (no Math.random for stable runs). */
function makeFixtures() {
  const words =
    'component render props state effect memo callback context reducer selector dispatch module export import default async await promise buffer stream'.split(
      ' ',
    )
  let state = 0x1badb002
  const rand = () => {
    state ^= state << 13
    state ^= state >>> 17
    state ^= state << 5
    return (state >>> 0) / 0xffffffff
  }

  // Long-tail distribution, roughly matching a production dist/ folder.
  const buckets = [
    { count: 140, min: 2 * 1024, max: 24 * 1024 }, // route chunks, css, json
    { count: 45, min: 60 * 1024, max: 300 * 1024 }, // feature bundles
    { count: 15, min: 1.5 * 1024 * 1024, max: 4.5 * 1024 * 1024 }, // vendor bundles
  ]

  const files = []
  let index = 0
  for (const bucket of buckets) {
    for (let i = 0; i < Math.max(1, Math.round(bucket.count * SCALE)); i++) {
      const target = Math.floor(bucket.min + rand() * (bucket.max - bucket.min))
      const lines = []
      let size = 0
      while (size < target) {
        const a = words[Math.floor(rand() * words.length)]
        const b = words[Math.floor(rand() * words.length)]
        const line = `export function ${a}_${b}_${index}(input) { return input.${a}?.${b} ?? ${Math.floor(rand() * 1e6)}; }\n`
        lines.push(line)
        size += line.length
      }
      files.push({ name: `chunk-${index}.js`, data: Buffer.from(lines.join('')) })
      index++
    }
  }
  return files
}

async function runNative(files, tasks) {
  const started = performance.now()
  const results = await compressBuffers(
    tasks.map(({ fileIndex, algorithm, level }) => ({
      fileName: files[fileIndex].name,
      algorithm,
      level,
    })),
    tasks.map(({ fileIndex }) => files[fileIndex].data),
  )
  const elapsed = performance.now() - started
  for (const result of results) {
    if (result.error) throw new Error(`${result.fileName}: ${result.error}`)
  }
  return { elapsed, outputBytes: results.reduce((sum, r) => sum + r.compressedSize, 0) }
}

function jsCompress(data, algorithm, level) {
  switch (algorithm) {
    case 'gzip':
      return gzipAsync(data, { level })
    case 'brotli':
      return brotliAsync(data, {
        params: {
          [zlib.constants.BROTLI_PARAM_QUALITY]: level,
          [zlib.constants.BROTLI_PARAM_SIZE_HINT]: data.byteLength,
        },
      })
    case 'zstd':
      if (!zstdAsync) return null
      return zstdAsync(data, {
        params: { [zlib.constants.ZSTD_c_compressionLevel]: level },
      })
    default:
      throw new Error(`unknown algorithm ${algorithm}`)
  }
}

async function runNodeZlib(files, tasks) {
  const started = performance.now()
  const jobs = tasks.map(({ fileIndex, algorithm, level }) =>
    jsCompress(files[fileIndex].data, algorithm, level),
  )
  if (jobs.some((job) => job === null)) return null
  const outputs = await Promise.all(jobs)
  const elapsed = performance.now() - started
  return { elapsed, outputBytes: outputs.reduce((sum, out) => sum + out.byteLength, 0) }
}

function makeTasks(files, algorithms) {
  return files.flatMap((_, fileIndex) =>
    algorithms.map(({ algorithm, level }) => ({ fileIndex, algorithm, level })),
  )
}

const files = makeFixtures()
const inputBytes = files.reduce((sum, file) => sum + file.data.byteLength, 0)
const formatMb = (bytes) => `${(bytes / 1024 / 1024).toFixed(2)} MB`

console.log(
  `fixtures: ${files.length} files, ${formatMb(inputBytes)} total | cpu cores: ${availableParallelism()} | UV_THREADPOOL_SIZE: ${process.env.UV_THREADPOOL_SIZE ?? '4 (default)'} | node ${process.version}${QUICK ? ' | quick mode' : ''}\n`,
)

// gzip 9 + brotli 11 are vite-plugin-compression2's defaults; the same levels
// are used on both sides of every row.
const scenarios = [
  {
    label: 'gzip+brotli (ref. defaults: 9/11)',
    algorithms: [
      { algorithm: 'gzip', level: 9 },
      { algorithm: 'brotli', level: 11 },
    ],
  },
  { label: 'gzip (level 9)', algorithms: [{ algorithm: 'gzip', level: 9 }] },
  { label: 'gzip (level 6)', algorithms: [{ algorithm: 'gzip', level: 6 }] },
  { label: 'brotli (quality 11)', algorithms: [{ algorithm: 'brotli', level: 11 }] },
  { label: 'brotli (quality 6)', algorithms: [{ algorithm: 'brotli', level: 6 }] },
  { label: 'zstd (level 19)', algorithms: [{ algorithm: 'zstd', level: 19 }] },
]

const rows = []
for (const scenario of scenarios) {
  const tasks = makeTasks(files, scenario.algorithms)
  const native = await runNative(files, tasks)
  const js = await runNodeZlib(files, tasks)
  rows.push({
    label: scenario.label,
    output: formatMb(native.outputBytes),
    native: native.elapsed,
    js: js?.elapsed ?? null,
  })
}

console.log('| scenario | output | native (rust) | node:zlib | speedup |')
console.log('|---|---|---|---|---|')
for (const row of rows) {
  const nativeS = `${(row.native / 1000).toFixed(2)}s`
  const jsS = row.js === null ? 'n/a' : `${(row.js / 1000).toFixed(2)}s`
  const speedup = row.js === null ? 'n/a' : `${(row.js / row.native).toFixed(2)}x`
  console.log(`| ${row.label} | ${row.output} | ${nativeS} | ${jsS} | ${speedup} |`)
}
