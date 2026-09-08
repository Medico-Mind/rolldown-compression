import { describe, expect, it } from 'vitest'

import { type CompressTask, compressBuffers } from '../../ts/binding.js'

describe('native error conversion', () => {
  it.each<[Partial<CompressTask>, string]>([
    [{ algorithm: 'lzma' }, 'unknown algorithm `lzma`, expected one of: gzip, brotli, zstd'],
    [{ algorithm: 'gzip', level: 10 }, 'invalid gzip level 10: expected 0..=9'],
    [{ algorithm: 'brotli', level: 12 }, 'invalid brotli level 12: expected 0..=11'],
    [{ algorithm: 'zstd', level: 0 }, 'invalid zstd level 0: expected 1..=22'],
    [{ windowBits: 9 }, 'invalid brotli windowBits 9: expected 10..=24'],
    [{ sectionSize: 0 }, 'invalid brotli sectionSize 0: expected a positive number of bytes'],
  ])('throws InvalidArg synchronously for %j', (options, message) => {
    expect(() =>
      compressBuffers(
        [{ fileName: 'test.js', algorithm: 'brotli', ...options }],
        [Buffer.from('x')],
      ),
    ).toThrowError(expect.objectContaining({ code: 'InvalidArg', message }))
  })

  it('reports mismatched batch lengths', () => {
    expect(() => compressBuffers([{ fileName: 'test.js', algorithm: 'gzip' }], [])).toThrowError(
      expect.objectContaining({
        code: 'InvalidArg',
        message: 'tasks and buffers must have the same length (got 1 tasks, 0 buffers)',
      }),
    )
  })
})
