import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { gunzipSync } from 'node:zlib'

import { afterAll, beforeEach, describe, expect, it, vi } from 'vitest'

import { resolveOptions } from '../../ts/options.js'
import { createCompressionPlugin } from '../../ts/plugin.js'

// Wrap the real native binding so every test still exercises actual
// compression while we observe how stream mode batches the FFI calls.
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

const tempDirs: string[] = []

afterAll(async () => {
  await Promise.all(tempDirs.map((dir) => rm(dir, { recursive: true, force: true })))
})

async function makeOutDir(files: Record<string, string | Uint8Array>): Promise<string> {
  const dir = await mkdtemp(path.join(tmpdir(), 'rolldown-compression-stream-'))
  tempDirs.push(dir)
  for (const [name, content] of Object.entries(files)) {
    const target = path.join(dir, name)
    await mkdir(path.dirname(target), { recursive: true })
    await writeFile(target, content)
  }
  return dir
}

function runWriteBundle(
  plugin: ReturnType<typeof createCompressionPlugin>,
  dir: string,
  { watchMode = false } = {},
) {
  const context = {
    meta: { watchMode },
    error(error: Error): never {
      throw error
    },
  }
  const hook = plugin.writeBundle as {
    order: string
    handler: (this: typeof context, outputOptions: object) => Promise<void>
  }
  expect(hook.order).toBe('post')
  return hook.handler.call(context, { dir })
}

async function listFiles(dir: string): Promise<string[]> {
  const files = await readdir(dir, { recursive: true })
  return files.map((file) => file.split(path.sep).join('/')).sort()
}

describe('stream mode', () => {
  beforeEach(() => {
    state.batchSizes.length = 0
  })

  it('compresses files from disk and leaves generateBundle a no-op', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ stream: true, algorithms: ['gzip'], logLevel: 'silent' }),
    )

    const emitFile = vi.fn()
    const generate = plugin.generateBundle as unknown as (
      this: object,
      outputOptions: object,
      bundle: object,
    ) => Promise<void>
    await generate.call(
      { meta: { watchMode: false }, emitFile },
      {},
      { 'main.js': { type: 'chunk', code: 'const x = 1;\n'.repeat(100) } },
    )
    expect(emitFile).not.toHaveBeenCalled()

    const code = 'export const answer = 42;\n'.repeat(200)
    const dir = await makeOutDir({
      'main.js': code,
      'assets/data.json': JSON.stringify({ items: Array(100).fill('x') }),
      'image.png': new Uint8Array([1, 2, 3]),
    })
    await runWriteBundle(plugin, dir)

    const files = await listFiles(dir)
    expect(files).toContain('main.js.gz')
    expect(files).toContain('assets/data.json.gz')
    expect(files).not.toContain('image.png.gz')
    expect(files).toContain('main.js')

    const gz = await readFile(path.join(dir, 'main.js.gz'))
    expect(gunzipSync(gz).toString()).toBe(code)
  })

  it('flushes a batch every 4 MB of source bytes when chunkSize is 0', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ stream: true, algorithms: ['gzip'], logLevel: 'silent' }),
    )
    // Five 1.5 MB files: the running total crosses 4 MB at the third file,
    // so the first batch holds 3 files and the trailing flush the other 2.
    const megabyte = 1024 * 1024
    const dir = await makeOutDir(
      Object.fromEntries(
        Array.from({ length: 5 }, (_, index) => [
          `file-${index}.js`,
          `export const value = ${index};\n`.repeat((1.5 * megabyte) / 25),
        ]),
      ),
    )
    await runWriteBundle(plugin, dir)

    expect(state.batchSizes).toEqual([3, 2])
    expect(await listFiles(dir)).toContain('file-4.js.gz')
  })

  it('flushes by source bytes when chunkSize is positive', async () => {
    // Every file is larger than 1 byte, so each one flushes its own batch.
    const plugin = createCompressionPlugin(
      resolveOptions({ stream: true, chunkSize: 1, algorithms: ['gzip'], logLevel: 'silent' }),
    )
    const dir = await makeOutDir({
      'a.js': 'export const a = 1;\n'.repeat(100),
      'b.js': 'export const b = 2;\n'.repeat(100),
      'c.js': 'export const c = 3;\n'.repeat(100),
    })
    await runWriteBundle(plugin, dir)

    expect(state.batchSizes).toEqual([1, 1, 1])
  })

  it('respects threshold and never re-compresses compressed artifacts', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({
        stream: true,
        threshold: 1024,
        include: /.*/,
        algorithms: ['gzip'],
        logLevel: 'silent',
      }),
    )
    const dir = await makeOutDir({
      'small.js': 'x'.repeat(100),
      'large.js': 'const value = 1;\n'.repeat(200),
      'app.js.gz': 'pretend gzip data'.repeat(200),
    })
    await runWriteBundle(plugin, dir)

    const files = await listFiles(dir)
    expect(files).not.toContain('small.js.gz')
    expect(files).toContain('large.js.gz')
    expect(files).not.toContain('app.js.gz.gz')
  })

  it('removes originals from disk when deleteOriginalAssets is set', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({
        stream: true,
        deleteOriginalAssets: true,
        algorithms: ['gzip'],
        logLevel: 'silent',
      }),
    )
    const dir = await makeOutDir({
      'main.js': 'export default 1;\n'.repeat(100),
      'keep.png': new Uint8Array(64),
    })
    await runWriteBundle(plugin, dir)

    expect(await listFiles(dir)).toEqual(['keep.png', 'main.js.gz'])
  })

  it('is a no-op in watch mode by default', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({ stream: true, algorithms: ['gzip'], logLevel: 'silent' }),
    )
    const dir = await makeOutDir({ 'main.js': 'const x = 1;\n'.repeat(100) })
    await runWriteBundle(plugin, dir, { watchMode: true })

    expect(await listFiles(dir)).toEqual(['main.js'])
  })

  it('errors when filename resolves to the source name', async () => {
    const plugin = createCompressionPlugin(
      resolveOptions({
        stream: true,
        filename: (fileName) => fileName,
        algorithms: ['gzip'],
        logLevel: 'silent',
      }),
    )
    const dir = await makeOutDir({ 'main.js': 'const x = 1;' })
    await expect(runWriteBundle(plugin, dir)).rejects.toThrow(/resolved "main.js".*same name/)
  })
})
