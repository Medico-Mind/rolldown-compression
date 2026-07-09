/**
 * Benchmark: baseline release build vs the PGO (and, on Linux, BOLT)
 * optimized build of the native binding.
 *
 * Both bindings are produced by `npm run build:pgo` (scripts/pgo/build.mjs)
 * and compress the exact same fixture set. Iterations are interleaved
 * (baseline, optimized, baseline, ...) so thermal drift and background noise
 * hit both sides equally; the reported number is the median.
 *
 * Usage: node benchmark/pgo-compare.mjs [--quick]
 */
import fs from 'node:fs'
import { createRequire } from 'node:module'
import { availableParallelism } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { makeFixtures, makeTasks, scenarios } from './fixtures.mjs'

const require = createRequire(import.meta.url)
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const pgoDir = path.join(root, 'target', 'pgo')

const QUICK = process.argv.includes('--quick')
const SCALE = QUICK ? 0.2 : 1
const ITERATIONS = QUICK ? 3 : 5

const baselinePath = path.join(pgoDir, 'baseline.node')
const boltPath = path.join(pgoDir, 'bolt.node')
const pgoPath = fs.existsSync(boltPath) ? boltPath : path.join(pgoDir, 'pgo.node')

if (!fs.existsSync(baselinePath) || !fs.existsSync(pgoPath)) {
  console.error('missing target/pgo/*.node artifacts — run `npm run build:pgo` first')
  process.exit(1)
}

const bindings = [
  { label: 'baseline', compressBuffers: require(baselinePath).compressBuffers },
  {
    label: pgoPath.endsWith('bolt.node') ? 'pgo+bolt' : 'pgo',
    compressBuffers: require(pgoPath).compressBuffers,
  },
]

const files = makeFixtures(SCALE)
const inputBytes = files.reduce((sum, file) => sum + file.data.byteLength, 0)

console.log(
  `fixtures: ${files.length} files, ${(inputBytes / 1024 / 1024).toFixed(2)} MB total | cpu cores: ${availableParallelism()} | node ${process.version}${QUICK ? ' | quick mode' : ''}`,
)
console.log(`baseline: ${path.relative(root, baselinePath)}`)
console.log(`optimized: ${path.relative(root, pgoPath)}\n`)

async function timeOnce(compressBuffers, files, tasks) {
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
  return elapsed
}

const median = (values) => values.toSorted((a, b) => a - b)[Math.floor(values.length / 2)]

const rows = []
for (const scenario of scenarios) {
  const tasks = makeTasks(files, scenario.algorithms)
  const samples = bindings.map(() => [])
  // Warm up both bindings (JIT, thread pools, page cache), then interleave.
  for (const binding of bindings) await timeOnce(binding.compressBuffers, files, tasks)
  for (let iteration = 0; iteration < ITERATIONS; iteration++) {
    for (const [index, binding] of bindings.entries()) {
      samples[index].push(await timeOnce(binding.compressBuffers, files, tasks))
    }
  }
  rows.push({
    label: scenario.label,
    baseline: median(samples[0]),
    optimized: median(samples[1]),
  })
}

const optimizedLabel = bindings[1].label
console.log(`| scenario | baseline | ${optimizedLabel} | speedup |`)
console.log('|---|---|---|---|')
for (const row of rows) {
  console.log(
    `| ${row.label} | ${(row.baseline / 1000).toFixed(2)}s | ${(row.optimized / 1000).toFixed(2)}s | ${(row.baseline / row.optimized).toFixed(2)}x |`,
  )
}
