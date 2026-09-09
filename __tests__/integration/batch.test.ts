import { brotliDecompressSync, gunzipSync, zstdDecompressSync } from 'node:zlib'

import { describe, expect, it } from 'vitest'

import { type CompressAlgorithm, compressBuffers } from '../../ts/binding.js'

const algorithms: CompressAlgorithm[] = [
  { algorithm: 'gzip', level: 1 },
  { algorithm: 'brotli', level: 4, windowBits: 18, sectionSize: 64 * 1024 },
  { algorithm: 'zstd', level: 3 },
  { algorithm: 'gzip', level: 9 },
]
const decode = [gunzipSync, brotliDecompressSync, zstdDecompressSync, gunzipSync]

describe('native file batches', () => {
  it('applies every configuration to each file and preserves file then algorithm order', async () => {
    const padded = Buffer.from(`prefix${'export const value = 42;\n'.repeat(10_000)}suffix`)
    const files = [
      { fileName: 'empty.js', data: Buffer.alloc(0) },
      { fileName: 'large.js', data: padded.subarray(6, -6) },
      { fileName: 'small.js', data: Buffer.from('different content') },
    ]
    const results = await compressBuffers(files, algorithms)
    expect(results).toHaveLength(files.length * algorithms.length)
    for (const [fileIndex, file] of files.entries()) {
      for (const [algorithmIndex, config] of algorithms.entries()) {
        const result = results[fileIndex * algorithms.length + algorithmIndex]
        expect(result).toMatchObject({
          fileName: file.fileName,
          algorithm: config.algorithm,
          originalSize: file.data.length,
          skipped: false,
        })
        expect(result?.error).toBeUndefined()
        expect(decode[algorithmIndex]?.(result?.data ?? Buffer.alloc(0))).toEqual(file.data)
        // Compare with an isolated call to catch configuration or ordering mixups,
        // including the two gzip levels, whose result metadata is identical.
        const [single] = await compressBuffers([file], [config])
        expect(result?.data).toEqual(single?.data)
      }
    }
  })

  it('skips all growing variants without dropping results for another file', async () => {
    const files = [
      { fileName: 'tiny.js', data: Buffer.from('x') },
      { fileName: 'large.js', data: Buffer.from('repeated text '.repeat(1000)) },
    ]
    const results = await compressBuffers(files, algorithms, { skipIfLargerOrEqual: true })
    expect(results).toHaveLength(8)
    for (const result of results.slice(0, 4)) {
      expect(result).toMatchObject({ fileName: 'tiny.js', skipped: true, compressedSize: 0 })
      expect(result.data.length).toBe(0)
      expect(result.error).toBeUndefined()
    }
    for (const [index, result] of results.slice(4).entries()) {
      expect(result.skipped).toBe(false)
      expect(decode[index]?.(result.data)).toEqual(files[1]?.data)
    }
  })

  it('returns no results when either files or algorithms are empty', async () => {
    expect(await compressBuffers([], algorithms)).toEqual([])
    expect(await compressBuffers([{ fileName: 'a.js', data: Buffer.from('a') }], [])).toEqual([])
    expect(await compressBuffers([], [])).toEqual([])
  })
})
