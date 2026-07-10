import { createHash } from 'node:crypto'

import { describe, expect, it } from 'vitest'

import { compression, defineAlgorithm } from '../../ts/index.js'
import {
  DEFAULT_INCLUDE,
  normalizeAlgorithmName,
  resolveOptions,
  resolveOutputFileName,
} from '../../ts/options.js'

describe('defineAlgorithm', () => {
  it('normalizes aliases to canonical names', () => {
    expect(defineAlgorithm('gz').algorithm).toBe('gzip')
    expect(defineAlgorithm('gzip').algorithm).toBe('gzip')
    expect(defineAlgorithm('br').algorithm).toBe('brotli')
    expect(defineAlgorithm('brotliCompress').algorithm).toBe('brotli')
    expect(defineAlgorithm('brotli').algorithm).toBe('brotli')
    expect(defineAlgorithm('zstandard').algorithm).toBe('zstd')
    expect(defineAlgorithm('zstd').algorithm).toBe('zstd')
  })

  it('keeps provided options', () => {
    expect(defineAlgorithm('brotli', { quality: 9, windowBits: 20 }).options).toEqual({
      quality: 9,
      windowBits: 20,
    })
  })

  it('rejects unknown algorithm names', () => {
    expect(() => normalizeAlgorithmName('lzma')).toThrow(/unknown algorithm "lzma"/)
  })

  it('rejects out-of-range levels at definition time', () => {
    expect(() => defineAlgorithm('gzip', { level: 10 })).toThrow(/gzip level/)
    expect(() => defineAlgorithm('gzip', { level: -1 })).toThrow(/gzip level/)
    expect(() => defineAlgorithm('gzip', { level: 1.5 })).toThrow(/gzip level/)
    expect(() => defineAlgorithm('brotli', { quality: 12 })).toThrow(/brotli quality/)
    expect(() => defineAlgorithm('brotli', { windowBits: 9 })).toThrow(/brotli windowBits/)
    expect(() => defineAlgorithm('brotli', { windowBits: 25 })).toThrow(/brotli windowBits/)
    expect(() => defineAlgorithm('zstd', { level: 0 })).toThrow(/zstd level/)
    expect(() => defineAlgorithm('zstd', { level: 23 })).toThrow(/zstd level/)
  })

  it('accepts boundary levels', () => {
    expect(() => defineAlgorithm('gzip', { level: 0 })).not.toThrow()
    expect(() => defineAlgorithm('gzip', { level: 9 })).not.toThrow()
    expect(() => defineAlgorithm('brotli', { quality: 0 })).not.toThrow()
    expect(() => defineAlgorithm('brotli', { quality: 11 })).not.toThrow()
    expect(() => defineAlgorithm('zstd', { level: 1 })).not.toThrow()
    expect(() => defineAlgorithm('zstd', { level: 22 })).not.toThrow()
  })
})

describe('resolveOptions', () => {
  it('defaults to gzip + brotli with default levels', () => {
    const resolved = resolveOptions()
    expect(resolved.algorithms).toEqual([
      { algorithm: 'gzip', level: 6, extension: '.gz' },
      { algorithm: 'brotli', level: 11, extension: '.br' },
    ])
    expect(resolved.threshold).toBe(0)
    expect(resolved.skipIfLargerOrEqual).toBe(true)
    expect(resolved.deleteOriginalAssets).toBe(false)
    expect(resolved.concurrency).toBe(0)
    expect(resolved.chunkSize).toBe(0)
    expect(resolved.logLevel).toBe('info')
    expect(resolved.enableInWatchMode).toBe(false)
  })

  it('normalizes string shorthands and defineAlgorithm entries', () => {
    const resolved = resolveOptions({
      algorithms: ['gz', defineAlgorithm('brotli', { quality: 5, windowBits: 18 }), 'zstandard'],
    })
    expect(resolved.algorithms).toEqual([
      { algorithm: 'gzip', level: 6, extension: '.gz' },
      { algorithm: 'brotli', level: 5, windowBits: 18, extension: '.br' },
      { algorithm: 'zstd', level: 19, extension: '.zst' },
    ])
  })

  it('re-validates hand-constructed algorithm entries', () => {
    expect(() =>
      resolveOptions({ algorithms: [{ algorithm: 'zstd', options: { level: 99 } }] }),
    ).toThrow(/zstd level/)
    expect(() =>
      // biome-ignore lint/suspicious/noExplicitAny: deliberately malformed input
      resolveOptions({ algorithms: [42 as any] }),
    ).toThrow(/invalid algorithms entry/)
  })

  it('applies the documented default include filter', () => {
    const { filter } = resolveOptions()
    for (const name of ['a.js', 'a.mjs', 'a.css', 'a.html', 'a.svg', 'a.json', 'a.wasm']) {
      expect(filter(name), name).toBe(true)
    }
    for (const name of ['a.png', 'a.jpeg', 'a.woff2', 'a.gz', 'a.map']) {
      expect(filter(name), name).toBe(false)
    }
    expect(DEFAULT_INCLUDE.test('index.html')).toBe(true)
  })

  it('supports glob strings, RegExps and arrays in filters', () => {
    const globbed = resolveOptions({ include: 'assets/**/*.js' })
    expect(globbed.filter('assets/deep/file.js')).toBe(true)
    expect(globbed.filter('other/file.js')).toBe(false)

    const mixed = resolveOptions({ include: [/\.css$/, '**/*.js'] })
    expect(mixed.filter('style.css')).toBe(true)
    expect(mixed.filter('deep/nested/app.js')).toBe(true)
    expect(mixed.filter('image.png')).toBe(false)
  })

  it('lets exclude win over include', () => {
    const resolved = resolveOptions({ include: /\.js$/, exclude: /vendor/ })
    expect(resolved.filter('app.js')).toBe(true)
    expect(resolved.filter('vendor/app.js')).toBe(false)
  })

  it('rejects invalid top-level options', () => {
    expect(() => resolveOptions({ threshold: -1 })).toThrow(/threshold/)
    expect(() => resolveOptions({ threshold: Number.NaN })).toThrow(/threshold/)
    expect(() => resolveOptions({ concurrency: -2 })).toThrow(/concurrency/)
    expect(() => resolveOptions({ concurrency: 1.5 })).toThrow(/concurrency/)
    expect(() => resolveOptions({ chunkSize: -1 })).toThrow(/chunkSize/)
    expect(() => resolveOptions({ chunkSize: 1.5 })).toThrow(/chunkSize/)
    // biome-ignore lint/suspicious/noExplicitAny: deliberately malformed input
    expect(() => resolveOptions({ logLevel: 'debug' as any })).toThrow(/logLevel/)
    // biome-ignore lint/suspicious/noExplicitAny: deliberately malformed input
    expect(() => resolveOptions({ filename: 42 as any })).toThrow(/filename/)
    expect(() => resolveOptions({ algorithms: [] })).toThrow(/algorithms/)
    // biome-ignore lint/suspicious/noExplicitAny: deliberately malformed input
    expect(() => resolveOptions(null as any)).toThrow(/options must be an object/)
  })

  it('throws from compression() itself for bad options', () => {
    expect(() => compression({ algorithms: [defineAlgorithm('gzip'), 'nope' as never] })).toThrow(
      /unknown algorithm/,
    )
  })
})

describe('resolveOutputFileName', () => {
  const gzip = { algorithm: 'gzip', level: 6, extension: '.gz' } as const
  const source = Buffer.from('body { color: red }')

  it('appends the per-algorithm extension by default', () => {
    expect(resolveOutputFileName(undefined, 'assets/app.js', gzip, source)).toBe('assets/app.js.gz')
    expect(resolveOutputFileName(undefined, 'top-level.css', gzip, source)).toBe('top-level.css.gz')
  })

  it('replaces all supported tokens', () => {
    expect(
      resolveOutputFileName('[path][name].[hash][ext].gz', 'assets/app.min.js', gzip, source),
    ).toBe(`assets/app.min.${expectedHash()}.js.gz`)
    expect(resolveOutputFileName('[base].gzip', 'assets/app.js', gzip, source)).toBe('app.js.gzip')
  })

  it('supports the function form with the canonical algorithm name', () => {
    const name = resolveOutputFileName(
      (fileName, algorithm) => `${fileName}.${algorithm}`,
      'a.js',
      { algorithm: 'zstd', level: 19, extension: '.zst' },
      source,
    )
    expect(name).toBe('a.js.zstd')
  })

  function expectedHash(): string {
    return createHash('sha256').update(source).digest('hex').slice(0, 8)
  }
})
