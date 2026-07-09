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
import { makeFixtures, makeTasks, scenarios } from './fixtures.mjs'

const require = createRequire(import.meta.url)
const { compressBuffers } = require('../index.js')

const QUICK = process.argv.includes('--quick')
const SCALE = QUICK ? 0.2 : 1

const gzipAsync = promisify(zlib.gzip)
const brotliAsync = promisify(zlib.brotliCompress)
const zstdAsync = zlib.zstdCompress ? promisify(zlib.zstdCompress) : null

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

const files = makeFixtures(SCALE)
const inputBytes = files.reduce((sum, file) => sum + file.data.byteLength, 0)
const formatMb = (bytes) => `${(bytes / 1024 / 1024).toFixed(2)} MB`

console.log(
  `fixtures: ${files.length} files, ${formatMb(inputBytes)} total | cpu cores: ${availableParallelism()} | UV_THREADPOOL_SIZE: ${process.env.UV_THREADPOOL_SIZE ?? '4 (default)'} | node ${process.version}${QUICK ? ' | quick mode' : ''}\n`,
)

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
