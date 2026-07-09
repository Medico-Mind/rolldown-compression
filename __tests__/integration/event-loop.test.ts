import { describe, expect, it } from 'vitest'

import { compressBuffers } from '../../ts/binding.js'

describe('event loop responsiveness', () => {
  it('keeps timers ticking while a heavy batch compresses natively', async () => {
    // ~32 MB of compressible text at brotli quality 11 takes seconds of CPU.
    const payload = Buffer.from('const answer = 42; // the meaning of life\n'.repeat(800_000))
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
