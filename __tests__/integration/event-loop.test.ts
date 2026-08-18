import { describe, expect, it } from 'vitest'

import { compressBuffers } from '../../ts/binding.js'

/**
 * Builds JS-shaped source that brotli cannot shortcut. Highly repetitive input
 * (e.g. one line repeated) collapses into a handful of enormous LZ matches, so
 * even quality 11 finishes in milliseconds and the batch never gets slow enough
 * for this test to prove anything. Structurally uniform lines carrying
 * high-entropy identifiers instead give the q11 match finder plenty of near
 * misses to evaluate and no cheap win, which is what makes it cost real CPU.
 *
 * Deterministic (xorshift32 from a fixed seed) so a failure reproduces exactly.
 */
function makeSource(lineCount: number): string {
  let seed = 0x9e3779b9
  const next = () => {
    seed ^= seed << 13
    seed >>>= 0
    seed ^= seed >>> 17
    seed ^= seed << 5
    seed >>>= 0
    return seed
  }

  const lines: string[] = []
  for (let index = 0; index < lineCount; index++) {
    lines.push(
      `export const sym_${next().toString(36)} = { id: ${index}, hash: '${next().toString(36)}${next().toString(36)}', tag: '${next().toString(16)}' }`,
    )
  }
  return lines.join('\n')
}

describe('event loop responsiveness', () => {
  it('keeps timers ticking while a heavy batch compresses natively', async () => {
    // ~1.6 MB of incompressible-ish source per chunk; brotli quality 11 spends
    // roughly a second of CPU on the batch, far above the 250 ms floor below.
    const payload = Buffer.from(makeSource(20_000))
    const tasks = Array.from({ length: 4 }, (_, index) => ({
      fileName: `chunk-${index}.js`,
      algorithm: 'brotli',
      level: 11,
    }))

    let ticks = 0
    let maxGapMs = 0
    let last = performance.now()
    const timer = setInterval(() => {
      const now = performance.now()
      maxGapMs = Math.max(maxGapMs, now - last)
      last = now
      ticks++
    }, 10)

    try {
      const started = performance.now()
      const results = await compressBuffers(
        tasks,
        tasks.map(() => payload),
      )
      const elapsed = performance.now() - started

      for (const result of results) {
        expect(result.error).toBeUndefined()
        expect(result.compressedSize).toBeGreaterThan(0)
        // Guard the premise: if this ever approaches 1.0 the payload became
        // trivially compressible again and the timing floor will follow.
        expect(result.compressedSize).toBeLessThan(payload.byteLength / 2)
      }
      // The batch must have been slow enough for the assertion to mean anything.
      expect(elapsed).toBeGreaterThan(250)
      expect(ticks).toBeGreaterThan(5)
      // A blocked event loop would produce a gap on the order of `elapsed`.
      expect(maxGapMs).toBeLessThan(500)
    } finally {
      clearInterval(timer)
    }
  })
})
