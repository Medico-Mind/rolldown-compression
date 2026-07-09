import { gunzipSync } from 'node:zlib'

import { describe, expect, it } from 'vitest'

import { compressBuffers } from '../../ts/binding.js'

/**
 * Memory-pressure test for very large single assets. Gated behind an env
 * flag because allocating hundreds of megabytes is unfriendly to shared CI
 * runners; enable with COMPRESSION_TEST_LARGE=1.
 */
describe.runIf(process.env.COMPRESSION_TEST_LARGE === '1')('large single asset', () => {
  it('compresses a >=100 MB asset without exhausting the JS heap', async () => {
    const chunk = 'export const filler = "abcdefghijklmnopqrstuvwxyz0123456789";\n'
    const payload = Buffer.from(chunk.repeat(Math.ceil((150 * 1024 * 1024) / chunk.length)))
    expect(payload.byteLength).toBeGreaterThanOrEqual(100 * 1024 * 1024)

    const [gz, zst] = await compressBuffers(
      [
        { fileName: 'huge.js', algorithm: 'gzip', level: 6 },
        { fileName: 'huge.js', algorithm: 'zstd', level: 3 },
      ],
      [payload, payload],
    )

    expect(gz?.error).toBeUndefined()
    expect(zst?.error).toBeUndefined()
    expect(gz?.originalSize).toBe(payload.byteLength)
    expect(gz && gz.compressedSize < payload.byteLength).toBe(true)
    // Spot-check integrity of the gzip stream.
    expect(gunzipSync(gz?.data ?? Buffer.alloc(0)).byteLength).toBe(payload.byteLength)
  })
})
