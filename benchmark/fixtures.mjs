/**
 * Shared benchmark fixtures and scenarios, used by both the native-vs-zlib
 * benchmark (index.mjs) and the PGO comparison benchmark (pgo-compare.mjs).
 */

/** Deterministic, code-shaped fixture content (no Math.random for stable runs). */
export function makeFixtures(scale = 1) {
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
    for (let i = 0; i < Math.max(1, Math.round(bucket.count * scale)); i++) {
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

// gzip 9 + brotli 11 are vite-plugin-compression2's defaults; the same levels
// are used on both sides of every row.
export const scenarios = [
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

export function makeTasks(files, algorithms) {
  return files.flatMap((_, fileIndex) =>
    algorithms.map(({ algorithm, level }) => ({ fileIndex, algorithm, level })),
  )
}
