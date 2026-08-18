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
 *
 * Rolldown runs the output hooks once per output, so the per-output state
 * below is reset in `renderStart` rather than shared across a whole build.
 */
import { mkdir, readdir, readFile, stat, unlink, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import path from 'node:path'

import type { Plugin } from 'rolldown'

import { type CompressTask, compressBuffers } from './binding.js'
import {
  type CanonicalAlgorithm,
  COMPRESSED_EXTENSION_RE,
  checkArtifactName,
  type LogLevel,
  type ResolvedOptions,
  resolveOutputFileName,
} from './options.js'

/**
 * Rolldown asks plugins to be named with a `rolldown-plugin-` prefix so they
 * are recognizable in logs, errors and hook traces.
 */
const PLUGIN_NAME = 'rolldown-plugin-compression'
const PACKAGE_NAME = '@medicomind/rolldown-compression'

/** Source bytes per batch in stream mode when `chunkSize` is 0. */
const STREAM_DEFAULT_CHUNK_SIZE = 4 * 1024 * 1024

/**
 * Slack applied when deciding whether a file on disk was written by the
 * current output. Some filesystems store mtimes at one-second granularity,
 * and the wall clock can drift against them, so a strict comparison would
 * occasionally misread a file the build had just produced as a leftover.
 */
const MTIME_SLACK_MS = 2_000

/** Concurrent `stat` calls while scanning the output directory. */
const STAT_CONCURRENCY = 32

/** Version of the shipping package, surfaced through `plugin.version`. */
const PACKAGE_VERSION = readPackageVersion()

function readPackageVersion(): string | undefined {
  try {
    // Both `ts/` (tests) and `dist/` (published) sit one directory below the
    // package root, so the relative specifier resolves in either layout.
    const { version } = createRequire(import.meta.url)('../package.json') as { version?: string }
    return typeof version === 'string' ? version : undefined
  } catch {
    return undefined
  }
}

/**
 * The plugin's `api`, for the documented inter-plugin communication channel:
 * another plugin can look this one up by name and learn which artifacts it
 * produces, e.g. to wire up a dev-server middleware that serves them.
 */
export interface CompressionPluginApi {
  /** Name of the package this plugin ships in. */
  readonly packageName: string
  /** Canonical algorithms this instance runs, in order. */
  readonly algorithms: readonly CanonicalAlgorithm[]
  /** Default extensions for those algorithms (`.gz`, `.br`, `.zst`). */
  readonly extensions: readonly string[]
  /** Artifact names emitted for the most recently generated output. */
  emittedFileNames(): string[]
}

/**
 * Fields Vite reads off a plugin object. They are not part of rolldown's
 * `Plugin` type — plain rolldown ignores them.
 */
interface VitePluginFields {
  apply?: 'build' | 'serve'
}

export type CompressionPlugin = Plugin<CompressionPluginApi> & VitePluginFields

function toBuffer(source: string | Uint8Array): Buffer {
  if (typeof source === 'string') return Buffer.from(source, 'utf8')
  if (Buffer.isBuffer(source)) return source
  // A view rather than a copy: the bytes are handed to the native module
  // within the same awaited call and nothing mutates the bundle in between,
  // so duplicating them here would only double peak memory.
  return Buffer.from(source.buffer, source.byteOffset, source.byteLength)
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

/**
 * The subset of the plugin context this module logs through. Every method is
 * optional so the plugin keeps working against hosts that only implement part
 * of the logging API.
 */
interface LoggingContext {
  debug?: (message: string) => void
  info?: (message: string) => void
  warn?: (message: string) => void
}

interface HookLogger {
  /** Per-file detail. Routed to `this.debug`, so the bundler's own log level decides visibility. */
  debug(message: string): void
  /** Once-per-build summaries. */
  info(message: string): void
  warn(message: string): void
}

function createLogger(context: LoggingContext, logLevel: LogLevel): HookLogger {
  const verbose = logLevel === 'info'
  const warns = logLevel === 'info' || logLevel === 'warn'
  const noop = () => {}
  return {
    debug: verbose ? (message) => context.debug?.(message) : noop,
    info: verbose ? (message) => context.info?.(message) : noop,
    warn: warns ? (message) => context.warn?.(message) : noop,
  }
}

interface BatchRunner {
  addFile(fileName: string, buffer: Buffer): Promise<void>
  flush(): Promise<void>
  readonly processedSources: Set<string>
  readonly emittedBySource: Map<string, number>
  readonly failures: string[]
  readonly stats: Map<string, AlgorithmStats>
}

interface BatchRunnerInit {
  options: ResolvedOptions
  log: HookLogger
  /** Artifact names emitted for the current output; added to as work completes. */
  emittedNames: Set<string>
  /** Names an artifact must not claim because the build already owns them. */
  isReserved: (fileName: string) => boolean
  emit: (artifact: PendingArtifact, data: Buffer) => void | Promise<void>
  fail: (message: string) => never
}

/**
 * Shared batching core: queues per-algorithm tasks, flushes them to the
 * native module and hands successful results to `emit`. Only one batch of
 * source buffers is referenced at a time once a flush trigger is set.
 */
function createBatchRunner({
  options,
  log,
  emittedNames,
  isReserved,
  emit,
  fail,
}: BatchRunnerInit): BatchRunner {
  const processedSources = new Set<string>()
  const emittedBySource = new Map<string, number>()
  const failures: string[] = []
  const stats = new Map<string, AlgorithmStats>()
  /** Artifact name -> the `source (algorithm)` that claimed it, for collision reporting. */
  const claimed = new Map<string, string>()

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
        log.debug(
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
      const check = checkArtifactName(
        resolveOutputFileName(options.filename, fileName, algorithm, buffer),
        fileName,
        algorithm.algorithm,
      )
      // `fail` throws, but only an explicit `return` narrows the union here.
      if (!check.ok) return fail(check.message)
      const outputFileName = check.fileName

      // Two artifacts writing the same name would silently drop one of them,
      // which normally means a `filename` function that ignores its algorithm
      // argument.
      const owner = claimed.get(outputFileName)
      if (owner !== undefined) {
        fail(
          `both ${owner} and ${fileName} (${algorithm.algorithm}) resolve to the artifact "${outputFileName}"; make the filename option depend on the algorithm`,
        )
      }
      if (isReserved(outputFileName)) {
        fail(
          `the artifact "${outputFileName}" for ${fileName} (${algorithm.algorithm}) would overwrite a file this build already owns`,
        )
      }
      claimed.set(outputFileName, `${fileName} (${algorithm.algorithm})`)

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

function formatSummary(stats: Map<string, AlgorithmStats>, startedAt: number): string | undefined {
  const summary = [...stats.entries()]
    .map(
      ([algorithm, stat]) =>
        `${algorithm}: ${stat.count} file(s), ${formatBytes(stat.originalBytes)} -> ${formatBytes(stat.outputBytes)}`,
    )
    .join('; ')
  if (summary.length === 0) return undefined
  const totalSaved = [...stats.values()].reduce(
    (sum, stat) => sum + (stat.originalBytes - stat.outputBytes),
    0,
  )
  const elapsedMs = performance.now() - startedAt
  return `${summary}; saved ${formatBytes(totalSaved)} in ${(elapsedMs / 1000).toFixed(2)}s`
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

/** Run `task` over `items` with at most `limit` in flight, preserving order. */
async function mapConcurrent<T, R>(
  items: readonly T[],
  limit: number,
  task: (item: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(items.length)
  let next = 0
  const worker = async () => {
    for (let index = next++; index < items.length; index = next++) {
      results[index] = await task(items[index] as T)
    }
  }
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker))
  return results
}

interface StreamSource {
  fileName: string
  absolute: string
  size: number
  /**
   * Whether this build produced the file: the bundle declares it, or it was
   * written while this output was being generated. Only these may be removed
   * by `deleteOriginalAssets`.
   */
  ownedByBuild: boolean
}

/**
 * Every file under `outDir` eligible for compression, each tagged with whether
 * this build produced it.
 *
 * Compression stays deliberately broad — whatever is in the output directory
 * and matches the filter gets compressed, which is the point of stream mode.
 * Deletion does not: removing a file is not undoable, so it is restricted to
 * files the build owns and never reaches leftovers from an earlier build in a
 * directory that was not emptied.
 */
async function collectStreamSources(
  outDir: string,
  bundleNames: ReadonlySet<string>,
  writtenSinceMs: number,
  options: ResolvedOptions,
  emittedNames: ReadonlySet<string>,
): Promise<StreamSource[]> {
  const absolutePaths = await walkFiles(outDir)
  const infos = await mapConcurrent(absolutePaths, STAT_CONCURRENCY, async (absolute) => {
    try {
      return await stat(absolute)
    } catch {
      // The file went away between the walk and the stat; nothing to compress.
      return undefined
    }
  })

  const sources: StreamSource[] = []
  for (const [index, absolute] of absolutePaths.entries()) {
    const info = infos[index]
    if (info === undefined) continue

    const fileName = path.relative(outDir, absolute).split(path.sep).join('/')
    if (!options.filter(fileName)) continue
    // Re-compression guard: never compress artifacts that are already
    // compressed, whether emitted by us or shipped as source assets.
    if (COMPRESSED_EXTENSION_RE.test(fileName) || emittedNames.has(fileName)) continue
    if (info.size < options.threshold) continue

    sources.push({
      fileName,
      absolute,
      size: info.size,
      ownedByBuild: bundleNames.has(fileName) || info.mtimeMs >= writtenSinceMs,
    })
  }
  return sources
}

/**
 * Split sources into read groups that mirror the batch boundaries
 * {@link createBatchRunner} would pick, so a group can be read concurrently
 * without ever holding more than one batch of file contents in memory.
 */
function* groupBySize(
  sources: readonly StreamSource[],
  maxBytes: number,
): Generator<StreamSource[]> {
  let group: StreamSource[] = []
  let bytes = 0
  for (const source of sources) {
    group.push(source)
    bytes += source.size
    if (bytes >= maxBytes) {
      yield group
      group = []
      bytes = 0
    }
  }
  if (group.length > 0) yield group
}

export function createCompressionPlugin(options: ResolvedOptions): CompressionPlugin {
  // Names emitted for the output currently being generated, so a second pass
  // never re-compresses our own artifacts. Rolldown runs the output hooks once
  // per output, so this is cleared in `renderStart` rather than accumulating
  // across every output of a build.
  const emittedNames = new Set<string>()
  /**
   * Wall-clock start of the current output. Stream mode compares file mtimes
   * against it to tell what this build wrote from what it merely found in the
   * output directory, which is what `deleteOriginalAssets` is allowed to touch.
   */
  let outputStartedAtMs: number | undefined
  /** `deleteOriginalAssets` removing a chunk is worth saying once, not every build. */
  let chunkDeletionWarned = false

  const warnDeletedChunks = (names: readonly string[], log: HookLogger) => {
    if (names.length === 0 || chunkDeletionWarned) return
    chunkDeletionWarned = true
    const sample = names.slice(0, 3).join(', ')
    log.warn(
      `deleteOriginalAssets removed ${names.length} chunk(s) (${sample}${names.length > 3 ? ', …' : ''}); ` +
        'requests for those paths only resolve if the server rewrites them to the compressed variant, ' +
        'and dynamic imports and source map links still reference the original names',
    )
  }

  const skipsWatchMode = (watchMode: boolean | undefined, log: HookLogger): boolean => {
    if (!watchMode || options.enableInWatchMode) return false
    log.info('watch mode detected, skipping compression (set enableInWatchMode to opt in)')
    return true
  }

  const plugin: CompressionPlugin = {
    name: PLUGIN_NAME,
    ...(PACKAGE_VERSION === undefined ? {} : { version: PACKAGE_VERSION }),

    // Lets tooling that inspects a build attribute the plugin to the package
    // it ships in, rather than to a bare name.
    meta: {
      packageName: PACKAGE_NAME,
      ...(PACKAGE_VERSION === undefined ? {} : { version: PACKAGE_VERSION }),
      description:
        'Compresses emitted assets with gzip, brotli and zstd through a native Rust core',
    },

    api: {
      packageName: PACKAGE_NAME,
      algorithms: options.algorithms.map((algorithm) => algorithm.algorithm),
      extensions: options.algorithms.map((algorithm) => algorithm.extension),
      emittedFileNames: () => [...emittedNames],
    },

    renderStart() {
      // One output's artifacts must not shadow another output's sources, and
      // in watch mode the set would otherwise grow for the life of the process.
      emittedNames.clear()
      outputStartedAtMs = Date.now()
    },

    async generateBundle(_outputOptions, bundle) {
      // Stream mode defers all work to `writeBundle`, once everything —
      // including assets other plugins write straight to disk — is there.
      if (options.stream) {
        // Fallback for hosts that do not call `renderStart`: this still runs
        // before the bundle is written, so it bounds the same window.
        outputStartedAtMs ??= Date.now()
        return
      }

      const log = createLogger(this, options.logLevel)
      if (skipsWatchMode(this.meta?.watchMode, log)) return

      const startedAt = performance.now()
      const fail = (message: string): never => this.error(new Error(`[${PLUGIN_NAME}] ${message}`))
      const runner = createBatchRunner({
        options,
        log,
        emittedNames,
        isReserved: (fileName) => Object.hasOwn(bundle, fileName),
        emit: (artifact, data) => {
          this.emitFile({
            type: 'asset',
            fileName: artifact.outputFileName,
            source: data,
          })
        },
        fail,
      })

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
        const deletedChunks: string[] = []
        for (const fileName of runner.processedSources) {
          // Dropping an asset that has no compressed variant — because every
          // algorithm skipped it, say — would leave the build with no copy of
          // it at all, so those are kept.
          if ((runner.emittedBySource.get(fileName) ?? 0) === 0) {
            log.warn(
              `kept "${fileName}": deleteOriginalAssets only removes an asset once a compressed variant of it exists`,
            )
            continue
          }
          if (bundle[fileName]?.type === 'chunk') deletedChunks.push(fileName)
          delete bundle[fileName]
        }
        warnDeletedChunks(deletedChunks, log)
      }

      const summary = formatSummary(runner.stats, startedAt)
      if (summary !== undefined) log.info(summary)
    },

    writeBundle: {
      // `post` so other plugins' `writeBundle` hooks have already written
      // their extra assets to disk before we scan the output directory.
      order: 'post',
      async handler(outputOptions, bundle) {
        if (!options.stream) return

        const log = createLogger(this, options.logLevel)
        if (skipsWatchMode(this.meta?.watchMode, log)) return

        const outDir =
          outputOptions.dir !== undefined
            ? path.resolve(outputOptions.dir)
            : outputOptions.file !== undefined
              ? path.resolve(path.dirname(outputOptions.file))
              : undefined
        if (outDir === undefined) {
          log.warn('stream mode could not determine the output directory, skipping compression')
          return
        }

        const startedAt = performance.now()
        const fail = (message: string): never =>
          this.error(new Error(`[${PLUGIN_NAME}] ${message}`))

        const bundleNames = new Set(Object.keys(bundle ?? {}))
        const sources = await collectStreamSources(
          outDir,
          bundleNames,
          // Without a start timestamp there is nothing to compare against, so
          // treat everything found as owned — the previous behavior.
          outputStartedAtMs === undefined ? -Infinity : outputStartedAtMs - MTIME_SLACK_MS,
          options,
          emittedNames,
        )
        // Compressing a file we are also about to overwrite would be a race
        // with ourselves; the sources are the one set of names artifacts may
        // never claim.
        const sourceNames = new Set(sources.map((source) => source.fileName))
        const ownedNames = new Set(
          sources.filter((source) => source.ownedByBuild).map((source) => source.fileName),
        )

        const runner = createBatchRunner({
          options,
          log,
          emittedNames,
          isReserved: (fileName) => sourceNames.has(fileName),
          emit: async (artifact, data) => {
            const target = path.join(outDir, artifact.outputFileName)
            await mkdir(path.dirname(target), { recursive: true })
            await writeFile(target, data)
          },
          fail,
        })

        // Contents are read a batch at a time and released once that batch has
        // been compressed, so the whole output directory is never resident.
        const batchBytes = options.chunkSize > 0 ? options.chunkSize : STREAM_DEFAULT_CHUNK_SIZE
        for (const group of groupBySize(sources, batchBytes)) {
          const buffers = await mapConcurrent(group, group.length, (source) =>
            readFile(source.absolute),
          )
          for (const [index, source] of group.entries()) {
            await runner.addFile(source.fileName, buffers[index] as Buffer)
          }
        }

        await runner.flush()

        if (runner.processedSources.size === 0) return

        if (runner.failures.length > 0) {
          fail(
            `${runner.failures.length} compression task(s) failed:\n${runner.failures.join('\n')}`,
          )
        }

        if (options.deleteOriginalAssets) {
          const deletedChunks: string[] = []
          for (const fileName of runner.processedSources) {
            if ((runner.emittedBySource.get(fileName) ?? 0) === 0) {
              log.warn(
                `kept "${fileName}": deleteOriginalAssets only removes an asset once a compressed variant of it exists`,
              )
              continue
            }
            // It was compressed, but this build did not put it there — most
            // likely a leftover in an output directory that was not emptied.
            // Compressing it is harmless; deleting it is not.
            if (!ownedNames.has(fileName)) {
              log.warn(
                `kept "${fileName}": deleteOriginalAssets only removes files this build wrote to the output directory`,
              )
              continue
            }
            if (bundle?.[fileName]?.type === 'chunk') deletedChunks.push(fileName)
            await unlink(path.join(outDir, fileName))
          }
          warnDeletedChunks(deletedChunks, log)
        }

        const summary = formatSummary(runner.stats, startedAt)
        if (summary !== undefined) log.info(summary)
      },
    },
  }

  // Vite / rolldown-vite only run `apply: 'build'` plugins for production
  // builds; plain rolldown ignores the field. Combined with the watch-mode
  // guard above this makes the plugin a build-only no-op by default.
  plugin.apply = 'build'
  return plugin
}
