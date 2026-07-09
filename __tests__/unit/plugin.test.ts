import { gunzipSync } from 'node:zlib'

import { describe, expect, it, vi } from 'vitest'

import { resolveOptions } from '../../ts/options.js'
import { createCompressionPlugin, createLogger } from '../../ts/plugin.js'

type Bundle = Record<
  string,
  { type: 'chunk'; code: string } | { type: 'asset'; source: string | Uint8Array }
>

interface EmittedAsset {
  type: 'asset'
  fileName: string
  source: Buffer
}

function runGenerateBundle(
  plugin: ReturnType<typeof createCompressionPlugin>,
  bundle: Bundle,
  { watchMode = false } = {},
) {
  const emitted: EmittedAsset[] = []
  const context = {
    meta: { watchMode },
    emitFile(file: EmittedAsset) {
      emitted.push(file)
    },
    error(error: Error): never {
      throw error
    },
  }
  const hook = plugin.generateBundle as unknown as (
    this: typeof context,
    outputOptions: object,
    bundle: Bundle,
  ) => Promise<void>
  return { emitted, done: hook.call(context, {}, bundle) }
}

describe('createCompressionPlugin', () => {
  it('compresses chunks and assets and leaves originals in place', async () => {
    const plugin = createCompressionPlugin(resolveOptions({ logLevel: 'silent' }))
    const code = 'export const answer = 42;\n'.repeat(200)
    const bundle: Bundle = {
      'main.js': { type: 'chunk', code },
      'data.json': { type: 'asset', source: JSON.stringify({ items: Array(100).fill('x') }) },
      'image.png': { type: 'asset', source: new Uint8Array([1, 2, 3]) },
    }
    const { emitted, done } = runGenerateBundle(plugin, bundle)
    await done

    const names = emitted.map((file) => file.fileName).sort()
    expect(names).toEqual(['data.json.br', 'data.json.gz', 'main.js.br', 'main.js.gz'])
    expect(Object.keys(bundle)).toContain('main.js')

    const gz = emitted.find((file) => file.fileName === 'main.js.gz')
    expect(gunzipSync(gz?.source ?? Buffer.alloc(0)).toString()).toBe(code)
  })

  it('is a no-op in watch mode by default and opt-in via enableInWatchMode', async () => {
    const bundle: Bundle = { 'main.js': { type: 'chunk', code: 'const x = 1;\n'.repeat(100) } }

    const disabled = createCompressionPlugin(resolveOptions({ logLevel: 'silent' }))
    const first = runGenerateBundle(disabled, bundle, { watchMode: true })
    await first.done
    expect(first.emitted).toHaveLength(0)

    const enabled = createCompressionPlugin(
      resolveOptions({ logLevel: 'silent', enableInWatchMode: true, algorithms: ['gzip'] }),
    )
    const second = runGenerateBundle(enabled, bundle, { watchMode: true })
    await second.done
    expect(second.emitted.map((file) => file.fileName)).toEqual(['main.js.gz'])
  })

  it('respects the threshold on the original size', async () => {
    const plugin = createCompressionPlugin(resolveOptions({ threshold: 1024, logLevel: 'silent' }))
    const bundle: Bundle = {
      'small.js': { type: 'chunk', code: 'x'.repeat(100) },
      'large.js': { type: 'chunk', code: 'const value = 1;\n'.repeat(200) },
    }
    const { emitted, done } = runGenerateBundle(plugin, bundle)
    await done
    const names = emitted.map((file) => file.fileName)
    expect(names).not.toContain('small.js.gz')
    expect(names).toContain('large.js.gz')
  })

  it('never re-compresses already compressed artifacts', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ include: /.*/, algorithms: ['gzip'], logLevel: 'silent' }),
    )
    const bundle: Bundle = {
      'app.js.gz': { type: 'asset', source: 'pretend gzip data'.repeat(50) },
      'app.js.br': { type: 'asset', source: 'pretend brotli data'.repeat(50) },
      'app.js.zst': { type: 'asset', source: 'pretend zstd data'.repeat(50) },
    }
    const { emitted, done } = runGenerateBundle(plugin, bundle)
    await done
    expect(emitted).toHaveLength(0)
  })

  it('drops originals when deleteOriginalAssets is set', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ deleteOriginalAssets: true, algorithms: ['gzip'], logLevel: 'silent' }),
    )
    const bundle: Bundle = {
      'main.js': { type: 'chunk', code: 'export default 1;\n'.repeat(100) },
      'keep.png': { type: 'asset', source: new Uint8Array(64) },
    }
    const { emitted, done } = runGenerateBundle(plugin, bundle)
    await done
    expect(emitted.map((file) => file.fileName)).toEqual(['main.js.gz'])
    expect(Object.keys(bundle)).toEqual(['keep.png'])
  })

  it('errors when filename resolves to the source name', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({
        filename: (fileName) => fileName,
        deleteOriginalAssets: true,
        logLevel: 'silent',
      }),
    )
    const bundle: Bundle = { 'main.js': { type: 'chunk', code: 'const x = 1;' } }
    const { done } = runGenerateBundle(plugin, bundle)
    await expect(done).rejects.toThrow(/resolved "main.js".*same name/)
  })

  it('does not emit results that would be larger when skipIfLargerOrEqual is on', async () => {
    const incompressible = new Uint8Array(2048)
    let state = 0x9e3779b9
    for (let i = 0; i < incompressible.length; i++) {
      state ^= state << 13
      state ^= state >>> 17
      state ^= state << 5
      incompressible[i] = state & 0xff
    }
    const bundle: Bundle = { 'noise.bin': { type: 'asset', source: incompressible } }

    const skipping = createCompressionPlugin(
      resolveOptions({ include: /\.bin$/, algorithms: ['gzip'], logLevel: 'silent' }),
    )
    const first = runGenerateBundle(skipping, { ...bundle })
    await first.done
    expect(first.emitted).toHaveLength(0)

    const emitting = createCompressionPlugin(
      resolveOptions({
        include: /\.bin$/,
        algorithms: ['gzip'],
        skipIfLargerOrEqual: false,
        logLevel: 'silent',
      }),
    )
    const second = runGenerateBundle(emitting, { ...bundle })
    await second.done
    expect(second.emitted.map((file) => file.fileName)).toEqual(['noise.bin.gz'])
    expect(second.emitted[0]?.source.byteLength).toBeGreaterThanOrEqual(incompressible.byteLength)
  })

  it('logs an info summary after compressing', async () => {
    const info = vi.spyOn(console, 'info').mockImplementation(() => {})
    try {
      const plugin = createCompressionPlugin(resolveOptions({ algorithms: ['gzip'] }))
      const bundle: Bundle = { 'main.js': { type: 'chunk', code: 'const x = 1;\n'.repeat(500) } }
      const { done } = runGenerateBundle(plugin, bundle)
      await done
      expect(info).toHaveBeenCalledWith(expect.stringMatching(/gzip: 1 file\(s\).*saved/))
    } finally {
      info.mockRestore()
    }
  })
})

describe('createLogger', () => {
  it('gates messages by level', () => {
    const info = vi.spyOn(console, 'info').mockImplementation(() => {})
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    try {
      const silent = createLogger('silent')
      silent.info('a')
      silent.warn('b')
      silent.error('c')
      expect(info).not.toHaveBeenCalled()
      expect(warn).not.toHaveBeenCalled()
      expect(error).not.toHaveBeenCalled()

      const warnLevel = createLogger('warn')
      warnLevel.info('a')
      warnLevel.warn('b')
      warnLevel.error('c')
      expect(info).not.toHaveBeenCalled()
      expect(warn).toHaveBeenCalledTimes(1)
      expect(error).toHaveBeenCalledTimes(1)

      const infoLevel = createLogger('info')
      infoLevel.info('a')
      expect(info).toHaveBeenCalledTimes(1)
    } finally {
      info.mockRestore()
      warn.mockRestore()
      error.mockRestore()
    }
  })
})
