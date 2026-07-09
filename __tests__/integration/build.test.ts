import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { brotliDecompressSync, gunzipSync } from 'node:zlib'

import { decompress as zstdDecompress } from 'fzstd'
import { type Plugin, rolldown } from 'rolldown'
import { afterAll, describe, expect, it } from 'vitest'

import { type CompressionOptions, compression } from '../../ts/index.js'

const FIXTURE_ENTRY = fileURLToPath(new URL('../fixtures/project/main.js', import.meta.url))

const tempDirs: string[] = []

afterAll(async () => {
  await Promise.all(tempDirs.map((dir) => rm(dir, { recursive: true, force: true })))
})

function incompressibleBytes(length: number): Uint8Array {
  const bytes = new Uint8Array(length)
  let state = 0x12345678
  for (let index = 0; index < length; index++) {
    state ^= state << 13
    state ^= state >>> 17
    state ^= state << 5
    bytes[index] = state & 0xff
  }
  return bytes
}

/** Emits extra assets so builds contain more than the entry chunk. */
function emitAssetsPlugin(): Plugin {
  return {
    name: 'test-emit-assets',
    buildStart() {
      this.emitFile({
        type: 'asset',
        fileName: 'assets/data.json',
        source: JSON.stringify({ payload: Array(500).fill('rolldown-compression') }),
      })
      this.emitFile({
        type: 'asset',
        fileName: 'assets/noise.bin',
        source: incompressibleBytes(4096),
      })
      this.emitFile({
        type: 'asset',
        fileName: 'pretend.gz',
        source: 'not really gzip, but the guard must leave it alone '.repeat(20),
      })
    },
  }
}

async function buildFixture(
  options?: CompressionOptions,
  extraPlugins: Plugin[] = [],
  compressionPlugins?: Plugin[],
): Promise<{ outDir: string; files: string[] }> {
  const outDir = await mkdtemp(path.join(tmpdir(), 'rolldown-compression-'))
  tempDirs.push(outDir)
  const bundle = await rolldown({
    input: FIXTURE_ENTRY,
    plugins: [
      emitAssetsPlugin(),
      ...extraPlugins,
      ...(compressionPlugins ?? [compression({ logLevel: 'silent', ...options })]),
    ],
  })
  await bundle.write({ dir: outDir, entryFileNames: 'main.js' })
  await bundle.close()
  const files = (await readdir(outDir, { recursive: true })).map((file) =>
    file.split(path.sep).join('/'),
  )
  return { outDir, files: files.sort() }
}

describe('rolldown integration', () => {
  it('emits .gz and .br artifacts that decompress byte-identical (default config)', async () => {
    const { outDir, files } = await buildFixture()

    expect(files).toContain('main.js.gz')
    expect(files).toContain('main.js.br')
    expect(files).toContain('assets/data.json.gz')
    expect(files).toContain('assets/data.json.br')
    // Not matched by the default include filter:
    expect(files).not.toContain('assets/noise.bin.gz')
    // Originals stay by default:
    expect(files).toContain('main.js')

    const original = await readFile(path.join(outDir, 'main.js'))
    const fromGzip = gunzipSync(await readFile(path.join(outDir, 'main.js.gz')))
    const fromBrotli = brotliDecompressSync(await readFile(path.join(outDir, 'main.js.br')))
    expect(fromGzip.equals(original)).toBe(true)
    expect(fromBrotli.equals(original)).toBe(true)
  })

  it('adds .zst artifacts when zstd is enabled and they decompress correctly', async () => {
    const { outDir, files } = await buildFixture({ algorithms: ['gzip', 'brotli', 'zstd'] })
    expect(files).toContain('main.js.zst')

    const original = await readFile(path.join(outDir, 'main.js'))
    const fromZstd = Buffer.from(
      zstdDecompress(new Uint8Array(await readFile(path.join(outDir, 'main.js.zst')))),
    )
    expect(fromZstd.equals(original)).toBe(true)
  })

  it('respects threshold', async () => {
    const { files } = await buildFixture({ threshold: 10 * 1024 * 1024 })
    expect(files.some((file) => file.endsWith('.gz') && file !== 'pretend.gz')).toBe(false)
    expect(files.some((file) => file.endsWith('.br'))).toBe(false)
  })

  it('respects skipIfLargerOrEqual for incompressible assets', async () => {
    const skipped = await buildFixture({ include: [/\.bin$/], algorithms: ['gzip'] })
    expect(skipped.files).not.toContain('assets/noise.bin.gz')

    const forced = await buildFixture({
      include: [/\.bin$/],
      algorithms: ['gzip'],
      skipIfLargerOrEqual: false,
    })
    expect(forced.files).toContain('assets/noise.bin.gz')
  })

  it('deleteOriginalAssets removes sources from the written output', async () => {
    const { files } = await buildFixture({ deleteOriginalAssets: true, algorithms: ['gzip'] })
    expect(files).not.toContain('main.js')
    expect(files).not.toContain('assets/data.json')
    expect(files).toContain('main.js.gz')
    expect(files).toContain('assets/data.json.gz')
    // Unmatched files survive:
    expect(files).toContain('assets/noise.bin')
  })

  it('never produces doubly-compressed artifacts across plugin instances', async () => {
    const { files } = await buildFixture(
      undefined,
      [],
      [
        compression({ logLevel: 'silent', algorithms: ['gzip'] }),
        compression({ logLevel: 'silent', algorithms: ['brotli'], include: /.*/ }),
      ],
    )
    expect(files).toContain('main.js.gz')
    expect(files).toContain('main.js.br')
    expect(files.some((file) => /\.(gz\.br|br\.gz|gz\.gz|zst\.(gz|br))$/.test(file))).toBe(false)
    expect(files).not.toContain('pretend.gz.br')
  })

  it('supports string patterns and function form filenames', async () => {
    const pattern = await buildFixture({ algorithms: ['gzip'], filename: '[path][base].gzip' })
    expect(pattern.files).toContain('main.js.gzip')

    const fn = await buildFixture({
      algorithms: ['zstd'],
      filename: (fileName, algorithm) => `${fileName}.${algorithm}.out`,
    })
    expect(fn.files).toContain('main.js.zstd.out')
  })

  it('fails the build when filename collides with the source asset', async () => {
    await expect(buildFixture({ algorithms: ['gzip'], filename: '[path][base]' })).rejects.toThrow(
      /same name/,
    )
  })
})
