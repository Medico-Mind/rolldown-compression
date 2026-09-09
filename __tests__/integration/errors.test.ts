import { describe, expect, it } from 'vitest'

import { type CompressAlgorithm, compressBuffers } from '../../ts/binding.js'

describe('native error conversion', () => {
  it.each<[Partial<CompressAlgorithm>, string]>([
    [{ algorithm: 'lzma' }, 'unknown algorithm `lzma`, expected one of: gzip, brotli, zstd'],
    [{ algorithm: 'gzip', level: 10 }, 'invalid gzip level 10: expected 0..=9'],
    [{ algorithm: 'brotli', level: 12 }, 'invalid brotli level 12: expected 0..=11'],
    [{ algorithm: 'zstd', level: 0 }, 'invalid zstd level 0: expected 1..=22'],
    [{ windowBits: 9 }, 'invalid brotli windowBits 9: expected 10..=24'],
    [{ sectionSize: 0 }, 'invalid brotli sectionSize 0: expected a positive number of bytes'],
  ])('throws InvalidArg synchronously for %j', (options, message) => {
    expect(() =>
      compressBuffers(
        [{ fileName: 'test.js', data: Buffer.from('x') }],
        [{ algorithm: 'brotli', ...options }],
      ),
    ).toThrowError(expect.objectContaining({ code: 'InvalidArg', message }))
  })

  it('validates algorithm settings even when the files array is empty', () => {
    expect(() => compressBuffers([], [{ algorithm: 'gzip', level: 10 }])).toThrowError(
      expect.objectContaining({
        code: 'InvalidArg',
        message: 'invalid gzip level 10: expected 0..=9',
      }),
    )
  })
})
