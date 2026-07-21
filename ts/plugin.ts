/**
 * Rolldown plugin implementation.
 *
 * Default mode: all eligible assets of a build are collected in
 * `generateBundle` (while they are still in memory) and compressed in
 * batched FFI calls; the native module fans the work out across a rayon
 * thread pool without ever blocking the JS event loop. By default
 * everything goes out in a single batch; a positive `chunkSize` flushes a
 * batch whenever its source bytes reach that limit, so only one batch of
 * buffer copies is alive at a time.
 *
 * Stream mode (`stream: true`): compression instead runs at the end of
 * `writeBundle` (order `'post'`), scanning the output directory on disk.
 * Files are read on demand and processed in bounded batches — `chunkSize`
 * source bytes per batch, falling back to
 * {@link STREAM_DEFAULT_CHUNK_SIZE} when `chunkSize` is 0 — so the whole
 * build is never held in memory and assets written to disk by other
 * plugins' `writeBundle` hooks are compressed as well.
 */
import { mkdir, readdir, readFile, stat, unlink, writeFile } from 'node:fs/promises'
import path from 'node:path'

import type { Plugin } from 'rolldown'

import { type CompressTask, compressBuffers } from './binding.js'
import {
  COMPRESSED_EXTENSION_RE,
  type LogLevel,
  type ResolvedOptions,
  resolveOutputFileName,
} from './options.js'

const PLUGIN_NAME = 'rolldown-compression'

/** Source bytes per batch in stream mode when `chunkSize` is 0. */
const STREAM_DEFAULT_CHUNK_SIZE = 4 * 1024 * 1024

interface Logger {
  info: (message: string) => void
  warn: (message: string) => void
  error: (message: string) => void
}

const LOG_PRIORITY: Record<LogLevel, number> = { silent: 0, error: 1, warn: 2, info: 3 }

export function createLogger(level: LogLevel): Logger {
  const priority = LOG_PRIORITY[level]
  const prefix = `[${PLUGIN_NAME}]`
  return {
    info: (message) => {
      if (priority >= LOG_PRIORITY.info) console.info(`${prefix} ${message}`)
    },
    warn: (message) => {
      if (priority >= LOG_PRIORITY.warn) console.warn(`${prefix} ${message}`)
    },
    error: (message) => {
      if (priority >= LOG_PRIORITY.error) console.error(`${prefix} ${message}`)
    },
  }
}

function toBuffer(source: string | Uint8Array): Buffer {
  return typeof source === 'string' ? Buffer.from(source, 'utf8') : Buffer.from(source)
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} kB`
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`
}

interface PendingArtifact {
  task: CompressTask
  buffer: Buffer
  sourceFileName: string
  outputFileName: string
}

interface AlgorithmStats {
  count: number
  originalBytes: number
  outputBytes: number
}

interface BatchRunner {
  addFile(fileName: string, buffer: Buffer): Promise<void>
  flush(): Promise<void>
  readonly processedSources: Set<string>
  readonly emittedBySource: Map<string, number>
  readonly failures: string[]
  readonly stats: Map<string, AlgorithmStats>
}

/**
 * Shared batching core: queues per-algorithm tasks, flushes them to the
 * native module and hands successful results to `emit`. Only one batch of
 * source buffers is referenced at a time once a flush trigger is set.
 */
function createBatchRunner(
  options: ResolvedOptions,
  logger: Logger,
  emittedNames: Set<string>,
  emit: (artifact: PendingArtifact, data: Buffer) => void | Promise<void>,
  fail: (message: string) => never,
): BatchRunner {
  const processedSources = new Set<string>()
  const emittedBySource = new Map<string, number>()
  const failures: string[] = []
  const stats = new Map<string, AlgorithmStats>()

  // Stream mode always batches: without it a chunkSize of 0 would buffer
  // the whole output directory, defeating on-demand processing.
  const chunkSize =
    options.chunkSize > 0 ? options.chunkSize : options.stream ? STREAM_DEFAULT_CHUNK_SIZE : 0

  let pending: PendingArtifact[] = []
  let pendingSourceBytes = 0

  const flush = async () => {
    if (pending.length === 0) return
    const batch = pending
    pending = []
    pendingSourceBytes = 0

    const results = await compressBuffers(
      batch.map((artifact) => artifact.task),
      batch.map((artifact) => artifact.buffer),
      {
        concurrency: options.concurrency,
        skipIfLargerOrEqual: options.skipIfLargerOrEqual,
      },
    )

    for (const [index, result] of results.entries()) {
      const artifact = batch[index]
      if (artifact === undefined) continue

      if (result.error !== undefined && result.error !== null) {
        failures.push(`${result.fileName} (${result.algorithm}): ${result.error}`)
        continue
      }
      if (result.skipped) {
        logger.info(
          `skipped ${artifact.outputFileName}: ${result.algorithm} output would not be smaller than the original`,
        )
        continue
      }

      await emit(artifact, result.data)
      emittedNames.add(artifact.outputFileName)
      emittedBySource.set(
        artifact.sourceFileName,
        (emittedBySource.get(artifact.sourceFileName) ?? 0) + 1,
      )

      const stat = stats.get(result.algorithm) ?? {
        count: 0,
        originalBytes: 0,
        outputBytes: 0,
      }
      stat.count += 1
      stat.originalBytes += result.originalSize
      stat.outputBytes += result.compressedSize
      stats.set(result.algorithm, stat)
    }
  }

  const addFile = async (fileName: string, buffer: Buffer) => {
    for (const algorithm of options.algorithms) {
      const outputFileName = resolveOutputFileName(options.filename, fileName, algorithm, buffer)
      if (outputFileName === fileName) {
        fail(
          `the filename option resolved "${fileName}" (${algorithm.algorithm}) to the same name as the source asset; refusing to overwrite it`,
        )
      }
      pending.push({
        task: {
          fileName,
          algorithm: algorithm.algorithm,
          level: algorithm.level,
          windowBits: algorithm.windowBits,
          sectionSize: algorithm.sectionSize,
        },
        buffer,
        sourceFileName: fileName,
        outputFileName,
      })
    }
    processedSources.add(fileName)

    pendingSourceBytes += buffer.byteLength
    if (chunkSize > 0 && pendingSourceBytes >= chunkSize) {
      await flush()
    }
  }

  return { addFile, flush, processedSources, emittedBySource, failures, stats }
}

function logSummary(logger: Logger, stats: Map<string, AlgorithmStats>, startedAt: number): void {
  const summary = [...stats.entries()]
    .map(
      ([algorithm, stat]) =>
        `${algorithm}: ${stat.count} file(s), ${formatBytes(stat.originalBytes)} -> ${formatBytes(stat.outputBytes)}`,
    )
    .join('; ')
  if (summary.length === 0) return
  const totalSaved = [...stats.values()].reduce(
    (sum, stat) => sum + (stat.originalBytes - stat.outputBytes),
    0,
  )
  const elapsedMs = performance.now() - startedAt
  logger.info(`${summary}; saved ${formatBytes(totalSaved)} in ${(elapsedMs / 1000).toFixed(2)}s`)
}

/** Recursively list every file under `root`, in a deterministic order. */
async function walkFiles(root: string): Promise<string[]> {
  const files: string[] = []
  const visit = async (dir: string) => {
    const entries = await readdir(dir, { withFileTypes: true })
    entries.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    for (const entry of entries) {
      const absolute = path.join(dir, entry.name)
      if (entry.isDirectory()) {
        await visit(absolute)
      } else if (entry.isFile()) {
        files.push(absolute)
      }
    }
  }
  await visit(root)
  return files
}

export function createCompressionPlugin(options: ResolvedOptions): Plugin {
  const logger = createLogger(options.logLevel)
  // Names emitted by this plugin instance, so a second pass (or a second
  // output) never re-compresses our own artifacts.
  const emittedNames = new Set<string>()

  const plugin: Plugin = {
    name: PLUGIN_NAME,

    async generateBundle(_outputOptions, bundle) {
      // Stream mode defers all work to `writeBundle`, once everything —
      // including assets other plugins write straight to disk — is there.
      if (options.stream) return

      if (this.meta?.watchMode && !options.enableInWatchMode) {
        logger.info('watch mode detected, skipping compression (set enableInWatchMode to opt in)')
        return
      }

      const startedAt = performance.now()
      const fail = (message: string): never => this.error(new Error(`[${PLUGIN_NAME}] ${message}`))
      const runner = createBatchRunner(
        options,
        logger,
        emittedNames,
        (artifact, data) => {
          this.emitFile({
            type: 'asset',
            fileName: artifact.outputFileName,
            source: data,
          })
        },
        fail,
      )

      for (const [fileName, output] of Object.entries(bundle)) {
        if (!options.filter(fileName)) continue
        // Re-compression guard: never compress artifacts that are already
        // compressed, whether emitted by us or shipped as source assets.
        if (COMPRESSED_EXTENSION_RE.test(fileName) || emittedNames.has(fileName)) continue

        const buffer = toBuffer(output.type === 'chunk' ? output.code : output.source)
        if (buffer.byteLength < options.threshold) continue

        await runner.addFile(fileName, buffer)
      }

      await runner.flush()

      if (runner.processedSources.size === 0) return

      if (runner.failures.length > 0) {
        fail(`${runner.failures.length} compression task(s) failed:\n${runner.failures.join('\n')}`)
      }

      if (options.deleteOriginalAssets) {
        for (const fileName of runner.processedSources) {
          if ((runner.emittedBySource.get(fileName) ?? 0) === 0) {
            logger.warn(
              `deleteOriginalAssets removed "${fileName}" even though no compressed variant was emitted for it`,
            )
          }
          delete bundle[fileName]
        }
      }

      logSummary(logger, runner.stats, startedAt)
    },

    writeBundle: {
      // `post` so other plugins' `writeBundle` hooks have already written
      // their extra assets to disk before we scan the output directory.
      order: 'post',
      async handler(outputOptions) {
        if (!options.stream) return

        if (this.meta?.watchMode && !options.enableInWatchMode) {
          logger.info('watch mode detected, skipping compression (set enableInWatchMode to opt in)')
          return
        }

        const outDir =
          outputOptions.dir !== undefined
            ? path.resolve(outputOptions.dir)
            : outputOptions.file !== undefined
              ? path.resolve(path.dirname(outputOptions.file))
              : undefined
        if (outDir === undefined) {
          logger.warn('stream mode could not determine the output directory, skipping compression')
          return
        }

        const startedAt = performance.now()
        const fail = (message: string): never =>
          this.error(new Error(`[${PLUGIN_NAME}] ${message}`))
        const runner = createBatchRunner(
          options,
          logger,
          emittedNames,
          async (artifact, data) => {
            const target = path.join(outDir, artifact.outputFileName)
            await mkdir(path.dirname(target), { recursive: true })
            await writeFile(target, data)
          },
          fail,
        )

        // The walk holds file names only; contents are read on demand and
        // released batch by batch.
        for (const absolute of await walkFiles(outDir)) {
          const fileName = path.relative(outDir, absolute).split(path.sep).join('/')
          if (!options.filter(fileName)) continue
          if (COMPRESSED_EXTENSION_RE.test(fileName) || emittedNames.has(fileName)) continue

          const info = await stat(absolute)
          if (info.size < options.threshold) continue

          await runner.addFile(fileName, await readFile(absolute))
        }

        await runner.flush()

        if (runner.processedSources.size === 0) return

        if (runner.failures.length > 0) {
          fail(
            `${runner.failures.length} compression task(s) failed:\n${runner.failures.join('\n')}`,
          )
        }

        if (options.deleteOriginalAssets) {
          for (const fileName of runner.processedSources) {
            if ((runner.emittedBySource.get(fileName) ?? 0) === 0) {
              logger.warn(
                `deleteOriginalAssets removed "${fileName}" even though no compressed variant was emitted for it`,
              )
            }
            await unlink(path.join(outDir, fileName))
          }
        }

        logSummary(logger, runner.stats, startedAt)
      },
    },
  }

  // Vite / rolldown-vite only run `apply: 'build'` plugins for production
  // builds; plain rolldown ignores the field. Combined with the watch-mode
  // guard above this makes the plugin a build-only no-op by default.
  return Object.assign(plugin, { apply: 'build' })
}
