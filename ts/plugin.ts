/**
 * Rolldown plugin implementation.
 *
 * All eligible assets of a build are collected in `generateBundle` (while
 * they are still in memory) and compressed in batched FFI calls; the native
 * module fans the work out across a rayon thread pool without ever blocking
 * the JS event loop. By default everything goes out in a single batch; a
 * positive `chunkSize` flushes a batch whenever its source bytes reach that
 * limit, so only one batch of buffer copies is alive at a time.
 */
import type { Plugin } from 'rolldown'

import { type CompressTask, compressBuffers } from './binding.js'
import {
  COMPRESSED_EXTENSION_RE,
  type LogLevel,
  type ResolvedOptions,
  resolveOutputFileName,
} from './options.js'

const PLUGIN_NAME = 'rolldown-compression'

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

export function createCompressionPlugin(options: ResolvedOptions): Plugin {
  const logger = createLogger(options.logLevel)
  // Names emitted by this plugin instance, so a second pass (or a second
  // output) never re-compresses our own artifacts.
  const emittedNames = new Set<string>()

  const plugin: Plugin = {
    name: PLUGIN_NAME,

    async generateBundle(_outputOptions, bundle) {
      if (this.meta?.watchMode && !options.enableInWatchMode) {
        logger.info('watch mode detected, skipping compression (set enableInWatchMode to opt in)')
        return
      }

      const startedAt = performance.now()
      const processedSources = new Set<string>()
      const failures: string[] = []
      const emittedBySource = new Map<string, number>()
      const stats = new Map<string, { count: number; originalBytes: number; outputBytes: number }>()

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

          this.emitFile({
            type: 'asset',
            fileName: artifact.outputFileName,
            source: result.data,
          })
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

      for (const [fileName, output] of Object.entries(bundle)) {
        if (!options.filter(fileName)) continue
        // Re-compression guard: never compress artifacts that are already
        // compressed, whether emitted by us or shipped as source assets.
        if (COMPRESSED_EXTENSION_RE.test(fileName) || emittedNames.has(fileName)) continue

        const buffer = toBuffer(output.type === 'chunk' ? output.code : output.source)
        if (buffer.byteLength < options.threshold) continue

        for (const algorithm of options.algorithms) {
          const outputFileName = resolveOutputFileName(
            options.filename,
            fileName,
            algorithm,
            buffer,
          )
          if (outputFileName === fileName) {
            this.error(
              new Error(
                `[${PLUGIN_NAME}] the filename option resolved "${fileName}" (${algorithm.algorithm}) to the same name as the source asset; refusing to overwrite it`,
              ),
            )
          }
          pending.push({
            task: {
              fileName,
              algorithm: algorithm.algorithm,
              level: algorithm.level,
              windowBits: algorithm.windowBits,
            },
            buffer,
            sourceFileName: fileName,
            outputFileName,
          })
        }
        processedSources.add(fileName)

        pendingSourceBytes += buffer.byteLength
        if (options.chunkSize > 0 && pendingSourceBytes >= options.chunkSize) {
          await flush()
        }
      }

      await flush()

      if (processedSources.size === 0) return

      if (failures.length > 0) {
        this.error(
          new Error(
            `[${PLUGIN_NAME}] ${failures.length} compression task(s) failed:\n${failures.join('\n')}`,
          ),
        )
      }

      if (options.deleteOriginalAssets) {
        for (const fileName of processedSources) {
          if ((emittedBySource.get(fileName) ?? 0) === 0) {
            logger.warn(
              `deleteOriginalAssets removed "${fileName}" even though no compressed variant was emitted for it`,
            )
          }
          delete bundle[fileName]
        }
      }

      const elapsedMs = performance.now() - startedAt
      const summary = [...stats.entries()]
        .map(
          ([algorithm, stat]) =>
            `${algorithm}: ${stat.count} file(s), ${formatBytes(stat.originalBytes)} -> ${formatBytes(stat.outputBytes)}`,
        )
        .join('; ')
      if (summary.length > 0) {
        const totalSaved = [...stats.values()].reduce(
          (sum, stat) => sum + (stat.originalBytes - stat.outputBytes),
          0,
        )
        logger.info(
          `${summary}; saved ${formatBytes(totalSaved)} in ${(elapsedMs / 1000).toFixed(2)}s`,
        )
      }
    },
  }

  // Vite / rolldown-vite only run `apply: 'build'` plugins for production
  // builds; plain rolldown ignores the field. Combined with the watch-mode
  // guard above this makes the plugin a build-only no-op by default.
  return Object.assign(plugin, { apply: 'build' })
}
