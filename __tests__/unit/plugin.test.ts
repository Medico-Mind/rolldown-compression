import { gunzipSync } from 'node:zlib'

import { describe, expect, it, vi } from 'vitest'

import { resolveOptions } from '../../ts/options.js'
import { createCompressionPlugin } from '../../ts/plugin.js'

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
  { watchMode = false, renderStart = true } = {},
) {
  const emitted: EmittedAsset[] = []
  const debug = vi.fn()
  const info = vi.fn()
  const warn = vi.fn()
  const context = {
    meta: { watchMode },
    debug,
    info,
    warn,
    emitFile(file: EmittedAsset) {
      emitted.push(file)
    },
    error(error: Error): never {
      throw error
    },
  }
  // Rolldown opens every output with `renderStart`; the plugin hangs its
  // per-output state off it.
  if (renderStart) {
    ;(plugin.renderStart as unknown as (this: typeof context) => void).call(context)
  }
  const hook = plugin.generateBundle as unknown as (
    this: typeof context,
    outputOptions: object,
    bundle: Bundle,
  ) => Promise<void>
  return { emitted, debug, info, warn, done: hook.call(context, {}, bundle) }
}

/** Bytes that gzip cannot shrink, so every algorithm skips them. */
function incompressibleBytes(length: number): Uint8Array {
  const bytes = new Uint8Array(length)
  let state = 0x9e3779b9
  for (let i = 0; i < length; i++) {
    state ^= state << 13
    state ^= state >>> 17
    state ^= state << 5
    bytes[i] = state & 0xff
  }
  return bytes
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
    const incompressible = incompressibleBytes(2048)
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
    const plugin = createCompressionPlugin(resolveOptions({ algorithms: ['gzip'] }))
    const bundle: Bundle = { 'main.js': { type: 'chunk', code: 'const x = 1;\n'.repeat(500) } }
    const { done, info } = runGenerateBundle(plugin, bundle)
    await done
    expect(info).toHaveBeenCalledWith(expect.stringMatching(/gzip: 1 file\(s\).*saved/))
  })

  it('reports per-file skips through debug rather than info', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ include: /\.bin$/, algorithms: ['gzip'] }),
    )
    const bundle: Bundle = { 'noise.bin': { type: 'asset', source: incompressibleBytes(2048) } }
    const { done, debug, info } = runGenerateBundle(plugin, bundle)
    await done

    expect(debug).toHaveBeenCalledWith(expect.stringMatching(/skipped noise\.bin\.gz/))
    expect(info).not.toHaveBeenCalledWith(expect.stringMatching(/skipped/))
  })

  it('describes itself the way the plugin conventions ask for', () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ algorithms: ['gzip', 'zstd'], logLevel: 'silent' }),
    )
    expect(plugin.name).toBe('rolldown-plugin-compression')
    expect(plugin.version).toMatch(/^\d+\.\d+\.\d+/)
    expect(plugin.meta?.packageName).toBe('@medicomind/rolldown-compression')
    expect(plugin.api?.algorithms).toEqual(['gzip', 'zstd'])
    expect(plugin.api?.extensions).toEqual(['.gz', '.zst'])
  })

  it('exposes the artifacts of the latest output through api', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ algorithms: ['gzip'], logLevel: 'silent' }),
    )
    await runGenerateBundle(plugin, {
      'main.js': { type: 'chunk', code: 'const x = 1;\n'.repeat(100) },
    }).done
    expect(plugin.api?.emittedFileNames()).toEqual(['main.js.gz'])

    // A second output starts from a clean slate, so one output's artifacts can
    // never shadow another output's sources.
    await runGenerateBundle(plugin, {
      'other.js': { type: 'chunk', code: 'const y = 2;\n'.repeat(100) },
    }).done
    expect(plugin.api?.emittedFileNames()).toEqual(['other.js.gz'])
  })

  describe('deleteOriginalAssets', () => {
    it('keeps assets that ended up without a compressed variant', async () => {
      const plugin = createCompressionPlugin(
        resolveOptions({
          include: /\.bin$/,
          deleteOriginalAssets: true,
          algorithms: ['gzip'],
        }),
      )
      const bundle: Bundle = { 'noise.bin': { type: 'asset', source: incompressibleBytes(2048) } }
      const { emitted, warn, done } = runGenerateBundle(plugin, bundle)
      await done

      expect(emitted).toHaveLength(0)
      expect(Object.keys(bundle)).toEqual(['noise.bin'])
      expect(warn).toHaveBeenCalledWith(expect.stringMatching(/kept "noise\.bin"/))
    })

    it('warns once when it removes chunks', async () => {
      const plugin = createCompressionPlugin(
        resolveOptions({ deleteOriginalAssets: true, algorithms: ['gzip'] }),
      )
      const makeBundle = (): Bundle => ({
        'main.js': { type: 'chunk', code: 'export default 1;\n'.repeat(100) },
        'data.json': { type: 'asset', source: JSON.stringify({ items: Array(100).fill('x') }) },
      })

      const first = runGenerateBundle(plugin, makeBundle())
      await first.done
      const chunkWarnings = first.warn.mock.calls.filter(([message]) =>
        /removed 1 chunk\(s\) \(main\.js\)/.test(String(message)),
      )
      expect(chunkWarnings).toHaveLength(1)

      const second = runGenerateBundle(plugin, makeBundle())
      await second.done
      expect(second.warn).not.toHaveBeenCalled()
    })
  })

  describe('artifact name validation', () => {
    const bundle = (): Bundle => ({
      'main.js': { type: 'chunk', code: 'const x = 1;\n'.repeat(100) },
    })

    it('rejects two algorithms resolving to the same artifact', async () => {
      const plugin = createCompressionPlugin(
        resolveOptions({
          filename: (fileName) => `${fileName}.compressed`,
          algorithms: ['gzip', 'brotli'],
          logLevel: 'silent',
        }),
      )
      await expect(runGenerateBundle(plugin, bundle()).done).rejects.toThrow(
        /both main\.js \(gzip\) and main\.js \(brotli\) resolve to the artifact "main\.js\.compressed"/,
      )
    })

    it('rejects artifacts that escape the output directory', async () => {
      const plugin = createCompressionPlugin(
        resolveOptions({
          filename: (fileName) => `../outside/${fileName}.gz`,
          algorithms: ['gzip'],
          logLevel: 'silent',
        }),
      )
      await expect(runGenerateBundle(plugin, bundle()).done).rejects.toThrow(
        /does not name a file inside the output directory/,
      )
    })

    it('rejects absolute artifact paths', async () => {
      const plugin = createCompressionPlugin(
        resolveOptions({
          filename: () => '/etc/app.js.gz',
          algorithms: ['gzip'],
          logLevel: 'silent',
        }),
      )
      await expect(runGenerateBundle(plugin, bundle()).done).rejects.toThrow(
        /absolute path "\/etc\/app\.js\.gz"/,
      )
    })

    it('rejects artifacts that would overwrite another file of the bundle', async () => {
      const plugin = createCompressionPlugin(
        resolveOptions({ include: /\.js$/, algorithms: ['gzip'], logLevel: 'silent' }),
      )
      const withCollision: Bundle = {
        'main.js': { type: 'chunk', code: 'const x = 1;\n'.repeat(100) },
        'main.js.gz': { type: 'asset', source: 'a hand-written artifact' },
      }
      await expect(runGenerateBundle(plugin, withCollision).done).rejects.toThrow(
        /"main\.js\.gz".*would overwrite a file this build already owns/,
      )
    })
  })
})
