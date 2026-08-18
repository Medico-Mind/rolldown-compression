/**
 * Option types, normalization and validation for the public API.
 *
 * Everything here is validated eagerly at `compression()` call time so that
 * misconfiguration fails when the config is evaluated, not mid-build.
 */
import { createHash } from 'node:crypto'
import path from 'node:path'

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

/**
 * Brotli options. `quality`: 0-11, default 11. `windowBits`: 10-24, default 22.
 * `sectionSize`: target bytes per worker thread when large inputs are split
 * across the native brotli worker pool; inputs at least twice this size
 * take the multithreaded path. Defaults to two windows (`2^(windowBits + 1)`
 * bytes), i.e. 8 MiB and multithreading from 16 MiB at the default window. Smaller
 * sections finish large files faster at a slight cost in compression ratio.
 */
export interface BrotliOptions {
  quality?: number
  windowBits?: number
  sectionSize?: number
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
   * Files to compress, matched against the output file name relative to the
   * output directory (always with `/` separators, never a leading `./`).
   * RegExps are tested against that name; strings are globs matched with
   * Node's `path.matchesGlob`, so they follow that dialect (`*` does not
   * cross `/`, `**` does).
   *
   * Note that source maps (`*.map`) are not covered by the default pattern;
   * add `/\.map$/` to `include` if you serve them pre-compressed.
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
   * leading dot), `[hash]` (8-char hash of the *original*, uncompressed
   * bytes). Function form receives the original file name and the canonical
   * algorithm name.
   *
   * Whichever form is used, the result must be a relative path inside the
   * output directory and must differ from the source name and from every
   * other artifact of the same build; anything else aborts the build.
   * @default '[path][base]' + per-algorithm extension (.gz / .br / .zst)
   */
  filename?: FilenameOption
  /**
   * Remove the original asset once every algorithm has produced a variant of
   * it. Files for which no variant was emitted — because `skipIfLargerOrEqual`
   * skipped them, for instance — are always kept, so this can never leave a
   * build without a servable copy of an asset. In `stream` mode it only ever
   * unlinks files this build wrote, never leftovers it merely found in the
   * output directory.
   *
   * Deleting chunks (JS/CSS) only works if whatever serves the build resolves
   * the original request path to the compressed variant; dynamic imports and
   * source map links still point at the original name. The plugin logs a
   * warning the first time it removes a chunk.
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
  /**
   * Maximum number of source bytes buffered per native compression batch.
   * `0` batches every asset into a single call. A positive value bounds the
   * plugin's peak memory overhead to roughly one batch of source copies plus
   * its compressed outputs; a single asset larger than `chunkSize` still
   * forms its own batch. Note the bundler keeps the original bundle in
   * memory regardless.
   * @default 0
   */
  chunkSize?: number
  /**
   * Compress from disk at the end of `writeBundle` instead of in memory in
   * `generateBundle`. The output directory is scanned and files are read on
   * demand in bounded batches (`chunkSize` source bytes per batch, or 4 MB
   * per batch when `chunkSize` is 0), so the whole build is never held in
   * memory and assets written to disk by other plugins' `writeBundle`
   * hooks are picked up as well.
   * @default false
   */
  stream?: boolean
  /**
   * `'info'` logs the per-build summary through `this.info` and per-file
   * detail through `this.debug` (the bundler's own log level decides whether
   * debug output is shown); `'warn'` keeps only warnings; `'error'` and
   * `'silent'` leave build-aborting errors as the only output.
   * @default 'info'
   */
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
  sectionSize?: number
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
  chunkSize: number
  stream: boolean
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

type FilterPattern = string | RegExp

function toArray(patterns: FilterPattern | FilterPattern[] | undefined): FilterPattern[] {
  if (patterns === undefined) return []
  return Array.isArray(patterns) ? patterns : [patterns]
}

function createFilter(
  include: FilterPattern | FilterPattern[] | undefined,
  exclude: FilterPattern | FilterPattern[] | undefined,
): (fileName: string) => boolean {
  const includes = toArray(include)
  const excludes = toArray(exclude)
  const matches = (fileName: string, pattern: FilterPattern) => {
    if (typeof pattern === 'string') return path.matchesGlob(fileName, pattern)
    pattern.lastIndex = 0
    return pattern.test(fileName)
  }

  return (fileName) => {
    if (typeof fileName !== 'string' || fileName.includes('\0')) return false
    const normalizedFileName = fileName.replaceAll('\\', '/')
    if (excludes.some((pattern) => matches(normalizedFileName, pattern))) return false
    return includes.length === 0 || includes.some((pattern) => matches(normalizedFileName, pattern))
  }
}

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
    const { quality, windowBits, sectionSize } = options as BrotliOptions
    if (quality !== undefined) {
      assertIntegerInRange(quality, LEVEL_RANGES.brotli, 'brotli quality')
    }
    if (windowBits !== undefined) {
      assertIntegerInRange(windowBits, [10, 24], 'brotli windowBits')
    }
    if (sectionSize !== undefined) {
      // Upper bound: the native module takes the size as a u32.
      assertIntegerInRange(sectionSize, [1, 4294967295], 'brotli sectionSize')
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
    const { windowBits, sectionSize } = options as BrotliOptions
    if (windowBits !== undefined) {
      resolved.windowBits = windowBits
    }
    if (sectionSize !== undefined) {
      resolved.sectionSize = sectionSize
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
    chunkSize = 0,
    stream = false,
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
  if (!Number.isInteger(chunkSize) || chunkSize < 0) {
    throw new OptionValidationError(
      `invalid chunkSize: expected a non-negative integer number of bytes, got ${JSON.stringify(chunkSize)}`,
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
    filter: createFilter(include, exclude),
    threshold,
    algorithms: algorithms.map(resolveAlgorithm),
    filename,
    deleteOriginalAssets: Boolean(deleteOriginalAssets),
    skipIfLargerOrEqual: Boolean(skipIfLargerOrEqual),
    concurrency,
    chunkSize,
    stream: Boolean(stream),
    logLevel,
    enableInWatchMode: Boolean(enableInWatchMode),
  }
}

/** Outcome of {@link checkArtifactName}. */
export type ArtifactNameCheck =
  | { readonly ok: true; readonly fileName: string }
  | { readonly ok: false; readonly message: string }

/**
 * Validate a resolved artifact name before anything is emitted or written.
 *
 * A `filename` function is arbitrary user code, so its result is checked the
 * same way a path from outside would be: it has to stay inside the output
 * directory and must not claim the name of the asset it was derived from.
 * Returns the normalized name to use, or the reason it was rejected.
 */
export function checkArtifactName(
  outputFileName: unknown,
  sourceFileName: string,
  algorithm: CanonicalAlgorithm,
): ArtifactNameCheck {
  const context = `the filename option resolved "${sourceFileName}" (${algorithm})`

  if (typeof outputFileName !== 'string' || outputFileName.length === 0) {
    return {
      ok: false,
      message: `${context} to ${JSON.stringify(outputFileName)}; expected a non-empty file name relative to the output directory`,
    }
  }
  if (outputFileName.includes('\0')) {
    return { ok: false, message: `${context} to a name containing a NUL byte` }
  }

  const normalized = path.posix.normalize(outputFileName.replaceAll('\\', '/'))
  if (path.posix.isAbsolute(normalized) || /^[a-zA-Z]:/.test(normalized)) {
    return {
      ok: false,
      message: `${context} to the absolute path "${outputFileName}"; expected a path relative to the output directory`,
    }
  }
  if (normalized === '..' || normalized.startsWith('../') || normalized.endsWith('/')) {
    return {
      ok: false,
      message: `${context} to "${outputFileName}", which does not name a file inside the output directory`,
    }
  }
  if (normalized === sourceFileName) {
    return {
      ok: false,
      message: `${context} to the same name as the source asset; refusing to overwrite it`,
    }
  }
  return { ok: true, fileName: normalized }
}

/**
 * Resolve the emitted file name for a compressed artifact.
 *
 * Supported tokens: `[path]`, `[base]`, `[name]`, `[ext]`, `[hash]`. The
 * result is not validated here — see {@link checkArtifactName}.
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
