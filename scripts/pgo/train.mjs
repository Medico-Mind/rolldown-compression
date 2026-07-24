/**
 * PGO / BOLT training workload.
 *
 * Loads an instrumented binding (path given as argv[2]) and drives every hot
 * path through the static corpus: all three algorithms at fast, default, and
 * max levels, brotli window-size variants, the skip-if-larger path, and both
 * the global and custom-sized rayon pools.
 *
 * Usage: node scripts/pgo/train.mjs <path/to/binding.node>
 */
import { createRequire } from 'node:module'
import path from 'node:path'
import { makeCorpus } from './corpus.mjs'

const bindingPath = process.argv[2]
if (!bindingPath) {
  console.error('usage: node scripts/pgo/train.mjs <path/to/binding.node>')
  process.exit(1)
}

const require = createRequire(import.meta.url)
const { compressBuffers } = require(path.resolve(bindingPath))

const corpus = makeCorpus()
const totalBytes = corpus.reduce((sum, file) => sum + file.data.byteLength, 0)
console.log(
  `training corpus: ${corpus.length} files, ${(totalBytes / 1024 / 1024).toFixed(2)} MB (${bindingPath})`,
)

const forFiles = (files, variants) =>
  files.flatMap((file) =>
    variants.map((variant) => ({
      task: { fileName: file.name, ...variant },
      data: file.data,
    })),
  )

const MB = 1024 * 1024
const smallAndMedium = corpus.filter((file) => file.data.byteLength <= 128 * 1024)
const medium = corpus.filter(
  (file) => file.data.byteLength > 8 * 1024 && file.data.byteLength <= 768 * 1024,
)
const vendorSized = corpus.filter(
  (file) => file.data.byteLength >= 2 * MB && file.data.byteLength < 16 * MB,
)
const incompressible = corpus.filter((file) => !file.compressible)

const batches = [
  {
    label: 'gzip fast/default/max',
    jobs: forFiles(corpus, [
      { algorithm: 'gzip', level: 1 },
      { algorithm: 'gzip', level: 6 },
      { algorithm: 'gzip', level: 9 },
    ]),
  },
  {
    label: 'zstd fast/default/max',
    jobs: forFiles(corpus, [
      { algorithm: 'zstd', level: 3 },
      { algorithm: 'zstd', level: 12 },
      { algorithm: 'zstd', level: 19 },
    ]),
  },
  {
    label: 'brotli fast/default',
    jobs: forFiles(corpus, [
      { algorithm: 'brotli', level: 4 },
      { algorithm: 'brotli', level: 6 },
    ]),
  },
  {
    // Quality 11 dominates real build times and uses different internal
    // paths for multi-MB inputs, so train it on the whole corpus including
    // the vendor-sized bundle (slow under instrumentation, but worth it).
    label: 'brotli max quality',
    jobs: forFiles(corpus, [{ algorithm: 'brotli', level: 11 }]),
  },
  {
    // The worker-pool path starts at 4x sectionSize (16 MiB with the 4 MiB
    // default). vendor-huge.js covers the default threshold in the batches
    // above; a small sectionSize pushes the remaining vendor-sized payloads
    // — including incompressible noise — through the same pool code without
    // needing more 16 MiB+ fixtures.
    label: 'brotli worker pool (custom sectionSize)',
    jobs: forFiles(vendorSized, [
      { algorithm: 'brotli', level: 6, sectionSize: 512 * 1024 },
      { algorithm: 'brotli', level: 11, sectionSize: 512 * 1024 },
    ]),
  },
  {
    label: 'brotli window variants',
    jobs: forFiles(medium, [
      { algorithm: 'brotli', level: 6, windowBits: 10 },
      { algorithm: 'brotli', level: 6, windowBits: 18 },
      { algorithm: 'brotli', level: 6, windowBits: 24 },
    ]),
  },
  {
    label: 'skip-if-larger path',
    options: { skipIfLargerOrEqual: true },
    jobs: forFiles(incompressible, [
      { algorithm: 'gzip', level: 9 },
      { algorithm: 'brotli', level: 6 },
      { algorithm: 'zstd', level: 19 },
    ]),
  },
  {
    label: 'custom thread pool',
    options: { concurrency: 2 },
    jobs: forFiles(smallAndMedium, [
      { algorithm: 'gzip', level: 6 },
      { algorithm: 'brotli', level: 6 },
      { algorithm: 'zstd', level: 12 },
    ]),
  },
]

for (const batch of batches) {
  const started = performance.now()
  const results = await compressBuffers(
    batch.jobs.map((job) => job.task),
    batch.jobs.map((job) => job.data),
    batch.options,
  )
  for (const result of results) {
    if (result.error) throw new Error(`${result.fileName}: ${result.error}`)
  }
  console.log(
    `  ${batch.label}: ${batch.jobs.length} tasks in ${((performance.now() - started) / 1000).toFixed(2)}s`,
  )
}

console.log('training complete')
