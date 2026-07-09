/**
 * @medicomind/rolldown-compression
 *
 * Rolldown plugin that compresses emitted assets with gzip, brotli and zstd
 * using a native Rust core (napi-rs + rayon).
 */
import type { Plugin } from 'rolldown'

import { type CompressionOptions, resolveOptions } from './options.js'
import { createCompressionPlugin } from './plugin.js'

export type {
  AlgorithmName,
  BrotliOptions,
  CanonicalAlgorithm,
  CompressionOptions,
  DefineAlgorithmResult,
  FilenameOption,
  GzipOptions,
  LogLevel,
  ZstdOptions,
} from './options.js'
export { defineAlgorithm } from './options.js'

/**
 * Create the compression plugin.
 *
 * Options are validated eagerly: invalid levels, algorithm names, thresholds
 * etc. throw here rather than during the build.
 *
 * @example
 * import { defineConfig } from 'rolldown'
 * import { compression, defineAlgorithm } from '@medicomind/rolldown-compression'
 *
 * export default defineConfig({
 *   plugins: [
 *     compression({
 *       threshold: 1024,
 *       algorithms: ['gzip', defineAlgorithm('brotli', { quality: 11 })],
 *     }),
 *   ],
 * })
 */
export function compression(options?: CompressionOptions): Plugin {
  return createCompressionPlugin(resolveOptions(options))
}
