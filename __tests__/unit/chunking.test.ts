import { gunzipSync } from 'node:zlib'

import { beforeEach, describe, expect, it, vi } from 'vitest'

import { resolveOptions } from '../../ts/options.js'
import { createCompressionPlugin } from '../../ts/plugin.js'

// Wrap the real native binding so every test still exercises actual
// compression while we observe how the plugin batches the FFI calls.
const state = vi.hoisted(() => ({ batchSizes: [] as number[] }))

vi.mock('../../ts/binding.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../ts/binding.js')>()
  return {
    ...actual,
    compressBuffers: (
      ...args: Parameters<typeof actual.compressBuffers>
    ): ReturnType<typeof actual.compressBuffers> => {
      state.batchSizes.push(args[0].length)
      return actual.compressBuffers(...args)
    },
  }
})

type Bundle = Record<
  string,
  { type: 'chunk'; code: string } | { type: 'asset'; source: string | Uint8Array }
>

interface EmittedAsset {
  type: 'asset'
  fileName: string
  source: Buffer
}

function runGenerateBundle(plugin: ReturnType<typeof createCompressionPlugin>, bundle: Bundle) {
  const emitted: EmittedAsset[] = []
  const context = {
    meta: { watchMode: false },
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

function makeBundle(): Bundle {
  return {
    'a.js': { type: 'chunk', code: 'export const a = 1;\n'.repeat(100) },
    'b.js': { type: 'chunk', code: 'export const b = 2;\n'.repeat(100) },
    'c.json': { type: 'asset', source: JSON.stringify({ items: Array(100).fill('c') }) },
  }
}

describe('chunkSize batching', () => {
  beforeEach(() => {
    state.batchSizes.length = 0
  })

  it('compresses everything in a single batch by default', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ algorithms: ['gzip'], logLevel: 'silent' }),
    )
    const { emitted, done } = runGenerateBundle(plugin, makeBundle())
    await done

    expect(state.batchSizes).toEqual([3])
    expect(emitted.map((file) => file.fileName).sort()).toEqual(['a.js.gz', 'b.js.gz', 'c.json.gz'])
  })

  it('flushes a batch whenever the buffered source bytes reach chunkSize', async () => {
    // Every file is larger than 1 byte, so each one flushes its own batch.
    const plugin = createCompressionPlugin(
      resolveOptions({ algorithms: ['gzip'], chunkSize: 1, logLevel: 'silent' }),
    )
    const { emitted, done } = runGenerateBundle(plugin, makeBundle())
    await done

    expect(state.batchSizes).toEqual([1, 1, 1])
    expect(emitted.map((file) => file.fileName).sort()).toEqual(['a.js.gz', 'b.js.gz', 'c.json.gz'])
  })

  it('keeps all algorithm variants of one file in the same batch', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ algorithms: ['gzip', 'brotli'], chunkSize: 1, logLevel: 'silent' }),
    )
    const { emitted, done } = runGenerateBundle(plugin, makeBundle())
    await done

    expect(state.batchSizes).toEqual([2, 2, 2])
    expect(emitted).toHaveLength(6)
  })

  it('groups files until the size limit and produces identical output', async () => {
    const bundle = makeBundle()
    const twoFilesBytes =
      Buffer.byteLength((bundle['a.js'] as { code: string }).code) +
      Buffer.byteLength((bundle['b.js'] as { code: string }).code)

    const plugin = createCompressionPlugin(
      resolveOptions({ algorithms: ['gzip'], chunkSize: twoFilesBytes, logLevel: 'silent' }),
    )
    const { emitted, done } = runGenerateBundle(plugin, bundle)
    await done

    expect(state.batchSizes).toEqual([2, 1])
    const gz = emitted.find((file) => file.fileName === 'a.js.gz')
    expect(gunzipSync(gz?.source ?? Buffer.alloc(0)).toString()).toBe(
      (bundle['a.js'] as { code: string }).code,
    )
  })
})
