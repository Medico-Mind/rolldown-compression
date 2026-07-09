/**
 * Option types, normalization and validation for the public API.
 *
 * Everything here is validated eagerly at `compression()` call time so that
 * misconfiguration fails when the config is evaluated, not mid-build.
 */
import { createHash } from 'node:crypto'
import path from 'node:path'

import { createFilter } from '@rollup/pluginutils'

/** Accepted algorithm names, including aliases. */
export type AlgorithmName =
  | 'gzip'
  | 'gz'
  | 'brotli'
  | 'br'
  | 'brotliCompress'
  | 'zstd'
  | 'zstandard'

/** Canonical algorithm names after alias normalization. */
export type CanonicalAlgorithm = 'gzip' | 'brotli' | 'zstd'

/** Gzip options. `level`: 0-9, default 6. */
export interface GzipOptions {
  level?: number
}

/** Brotli options. `quality`: 0-11, default 11. `windowBits`: 10-24, default 22. */
export interface BrotliOptions {
  quality?: number
  windowBits?: number
}

/** Zstandard options. `level`: 1-22, default 19. */
export interface ZstdOptions {
  level?: number
}

/** A normalized `(algorithm, options)` pair produced by {@link defineAlgorithm}. */
export interface DefineAlgorithmResult {
  readonly algorithm: CanonicalAlgorithm
  readonly options: GzipOptions | BrotliOptions | ZstdOptions
}

/** Pattern or callback used to derive compressed artifact names. */
export type FilenameOption = string | ((fileName: string, algorithm: CanonicalAlgorithm) => string)

export type LogLevel = 'silent' | 'error' | 'warn' | 'info'

/** Options accepted by {@link compression}. */
export interface CompressionOptions {
  /**
   * Files to compress. Strings are picomatch globs, RegExps are tested
   * against the output file name.
   * @default /\.(html|xml|css|json|js|mjs|svg|yaml|yml|toml|txt|wasm)$/
   */
  include?: string | RegExp | Array<string | RegExp>
  /** Files to exclude. Takes precedence over `include`. */
  exclude?: string | RegExp | Array<string | RegExp>
  /**
   * Minimum size of the original asset, in bytes, for it to be compressed.
   * @default 0
   */
  threshold?: number
  /**
   * Algorithms to run, as names or {@link defineAlgorithm} results.
   * @default ['gzip', 'brotli']
   */
  algorithms?: Array<AlgorithmName | DefineAlgorithmResult>
  /**
   * Name of the emitted artifact. Tokens: `[path]` (directory, with trailing
   * slash), `[base]` (file name with extension), `[name]`, `[ext]` (with
   * leading dot), `[hash]` (8-char content hash). Function form receives the
   * original file name and the canonical algorithm name.
   * @default '[path][base]' + per-algorithm extension (.gz / .br / .zst)
   */
  filename?: FilenameOption
  /**
   * Remove the original asset from the bundle once all algorithms have
   * processed it.
   * @default false
   */
  deleteOriginalAssets?: boolean
  /**
   * Do not emit artifacts whose compressed size is >= the original size.
   * @default true
   */
  skipIfLargerOrEqual?: boolean
  /**
   * Native worker threads used for compression. `0` = number of logical CPUs.
   * @default 0
   */
  concurrency?: number
  /** @default 'info' */
  logLevel?: LogLevel
  /**
   * The plugin is a no-op in watch/dev mode unless this is set to `true`.
   * @default false
   */
  enableInWatchMode?: boolean
}

/** A fully resolved algorithm entry ready to be sent to the native module. */
export interface ResolvedAlgorithm {
  algorithm: CanonicalAlgorithm
  level: number
  windowBits?: number
  extension: string
}

/** Internal, fully validated view of {@link CompressionOptions}. */
export interface ResolvedOptions {
  filter: (fileName: string) => boolean
  threshold: number
  algorithms: ResolvedAlgorithm[]
  filename?: FilenameOption
  deleteOriginalAssets: boolean
  skipIfLargerOrEqual: boolean
  concurrency: number
  logLevel: LogLevel
  enableInWatchMode: boolean
}

export const DEFAULT_INCLUDE = /\.(html|xml|css|json|js|mjs|svg|yaml|yml|toml|txt|wasm)$/

const ALIASES: Record<AlgorithmName, CanonicalAlgorithm> = {
  gzip: 'gzip',
  gz: 'gzip',
  brotli: 'brotli',
  br: 'brotli',
  brotliCompress: 'brotli',
  zstd: 'zstd',
  zstandard: 'zstd',
}

const EXTENSIONS: Record<CanonicalAlgorithm, string> = {
  gzip: '.gz',
  brotli: '.br',
  zstd: '.zst',
}

const DEFAULT_LEVELS: Record<CanonicalAlgorithm, number> = {
  gzip: 6,
  brotli: 11,
  zstd: 19,
}

const LEVEL_RANGES: Record<CanonicalAlgorithm, [number, number]> = {
  gzip: [0, 9],
  brotli: [0, 11],
  zstd: [1, 22],
}

const LOG_LEVELS: readonly LogLevel[] = ['silent', 'error', 'warn', 'info']

/** File extensions produced by this plugin; used by the re-compression guard. */
export const COMPRESSED_EXTENSION_RE = /\.(gz|br|zst)$/i

class OptionValidationError extends Error {
  constructor(message: string) {
    super(`[rolldown-compression] ${message}`)
    this.name = 'OptionValidationError'
  }
}

function assertIntegerInRange(value: unknown, [min, max]: [number, number], label: string): void {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) {
    throw new OptionValidationError(
      `invalid ${label}: expected an integer between ${min} and ${max}, got ${JSON.stringify(value)}`,
    )
  }
}

/** Normalize an algorithm name, throwing on unknown input. */
export function normalizeAlgorithmName(name: string): CanonicalAlgorithm {
  const canonical = ALIASES[name as AlgorithmName]
  if (canonical === undefined) {
    throw new OptionValidationError(
      `unknown algorithm ${JSON.stringify(name)}: expected one of ${Object.keys(ALIASES).join(', ')}`,
    )
  }
  return canonical
}

/**
 * Pair an algorithm with its options, validating both eagerly.
 *
 * @example
 * compression({ algorithms: [defineAlgorithm('brotli', { quality: 9 })] })
 */
export function defineAlgorithm(name: 'gzip' | 'gz', options?: GzipOptions): DefineAlgorithmResult
export function defineAlgorithm(
  name: 'brotli' | 'br' | 'brotliCompress',
  options?: BrotliOptions,
): DefineAlgorithmResult
export function defineAlgorithm(
  name: 'zstd' | 'zstandard',
  options?: ZstdOptions,
): DefineAlgorithmResult
export function defineAlgorithm(
  name: AlgorithmName,
  options: GzipOptions | BrotliOptions | ZstdOptions = {},
): DefineAlgorithmResult {
  const algorithm = normalizeAlgorithmName(name)
  validateAlgorithmOptions(algorithm, options)
  return Object.freeze({ algorithm, options: Object.freeze({ ...options }) })
}

function validateAlgorithmOptions(
  algorithm: CanonicalAlgorithm,
  options: GzipOptions | BrotliOptions | ZstdOptions,
): void {
  if (algorithm === 'brotli') {
    const { quality, windowBits } = options as BrotliOptions
    if (quality !== undefined) {
      assertIntegerInRange(quality, LEVEL_RANGES.brotli, 'brotli quality')
    }
    if (windowBits !== undefined) {
      assertIntegerInRange(windowBits, [10, 24], 'brotli windowBits')
    }
    return
  }
  const { level } = options as GzipOptions | ZstdOptions
  if (level !== undefined) {
    assertIntegerInRange(level, LEVEL_RANGES[algorithm], `${algorithm} level`)
  }
}

function resolveAlgorithm(entry: AlgorithmName | DefineAlgorithmResult): ResolvedAlgorithm {
  const { algorithm, options } =
    typeof entry === 'string'
      ? { algorithm: normalizeAlgorithmName(entry), options: {} }
      : validateDefined(entry)

  const resolved: ResolvedAlgorithm = {
    algorithm,
    level:
      algorithm === 'brotli'
        ? ((options as BrotliOptions).quality ?? DEFAULT_LEVELS.brotli)
        : ((options as GzipOptions | ZstdOptions).level ?? DEFAULT_LEVELS[algorithm]),
    extension: EXTENSIONS[algorithm],
  }
  if (algorithm === 'brotli') {
    const { windowBits } = options as BrotliOptions
    if (windowBits !== undefined) {
      resolved.windowBits = windowBits
    }
  }
  return resolved
}

function validateDefined(entry: DefineAlgorithmResult): DefineAlgorithmResult {
  if (
    entry === null ||
    typeof entry !== 'object' ||
    typeof entry.algorithm !== 'string' ||
    entry.options === null ||
    typeof entry.options !== 'object'
  ) {
    throw new OptionValidationError(
      `invalid algorithms entry ${JSON.stringify(entry)}: expected an algorithm name or a defineAlgorithm() result`,
    )
  }
  // Re-validate: the object may have been constructed by hand.
  const algorithm = normalizeAlgorithmName(entry.algorithm)
  validateAlgorithmOptions(algorithm, entry.options)
  return { algorithm, options: entry.options }
}

/** Validate and normalize user options. Throws at `compression()` call time. */
export function resolveOptions(options: CompressionOptions = {}): ResolvedOptions {
  if (options === null || typeof options !== 'object') {
    throw new OptionValidationError('options must be an object')
  }

  const {
    include = DEFAULT_INCLUDE,
    exclude,
    threshold = 0,
    algorithms = ['gzip', 'brotli'],
    filename,
    deleteOriginalAssets = false,
    skipIfLargerOrEqual = true,
    concurrency = 0,
    logLevel = 'info',
    enableInWatchMode = false,
  } = options

  if (typeof threshold !== 'number' || Number.isNaN(threshold) || threshold < 0) {
    throw new OptionValidationError(
      `invalid threshold: expected a non-negative number, got ${JSON.stringify(threshold)}`,
    )
  }
  if (!Number.isInteger(concurrency) || concurrency < 0) {
    throw new OptionValidationError(
      `invalid concurrency: expected a non-negative integer, got ${JSON.stringify(concurrency)}`,
    )
  }
  if (!LOG_LEVELS.includes(logLevel)) {
    throw new OptionValidationError(
      `invalid logLevel ${JSON.stringify(logLevel)}: expected one of ${LOG_LEVELS.join(', ')}`,
    )
  }
  if (filename !== undefined && typeof filename !== 'string' && typeof filename !== 'function') {
    throw new OptionValidationError('invalid filename: expected a string pattern or a function')
  }
  if (!Array.isArray(algorithms) || algorithms.length === 0) {
    throw new OptionValidationError('invalid algorithms: expected a non-empty array')
  }

  return {
    // `resolve: false` keeps matching relative to bundle file names instead
    // of resolving globs against the current working directory.
    filter: createFilter(include, exclude, { resolve: false }),
    threshold,
    algorithms: algorithms.map(resolveAlgorithm),
    filename,
    deleteOriginalAssets: Boolean(deleteOriginalAssets),
    skipIfLargerOrEqual: Boolean(skipIfLargerOrEqual),
    concurrency,
    logLevel,
    enableInWatchMode: Boolean(enableInWatchMode),
  }
}

/**
 * Resolve the emitted file name for a compressed artifact.
 *
 * Supported tokens: `[path]`, `[base]`, `[name]`, `[ext]`, `[hash]`.
 */
export function resolveOutputFileName(
  filename: FilenameOption | undefined,
  fileName: string,
  algorithm: ResolvedAlgorithm,
  source: Uint8Array,
): string {
  if (typeof filename === 'function') {
    return filename(fileName, algorithm.algorithm)
  }

  const pattern = filename ?? `[path][base]${algorithm.extension}`
  const dir = path.posix.dirname(fileName)
  const base = path.posix.basename(fileName)
  const ext = path.posix.extname(fileName)

  return pattern
    .replaceAll('[path]', dir === '.' ? '' : `${dir}/`)
    .replaceAll('[base]', base)
    .replaceAll('[name]', base.slice(0, base.length - ext.length))
    .replaceAll('[ext]', ext)
    .replaceAll('[hash]', () => createHash('sha256').update(source).digest('hex').slice(0, 8))
}
