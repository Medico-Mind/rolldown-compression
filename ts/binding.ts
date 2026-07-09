/**
 * Typed access to the native napi-rs module.
 *
 * The generated `index.js` loader (napi-rs) is CommonJS and lives at the
 * package root; both `ts/` (during tests) and `dist/` (after build) sit one
 * directory below it, so the relative specifier resolves in either layout.
 */
import { createRequire } from 'node:module'

import type { BatchOptions, CompressResult, CompressTask } from '../index.js'

const requireNative = createRequire(import.meta.url)

type NativeModule = {
  compressBuffers: (
    tasks: CompressTask[],
    buffers: Buffer[],
    options?: BatchOptions | undefined | null,
  ) => Promise<CompressResult[]>
}

const native = requireNative('../index.js') as NativeModule

/**
 * Compress a batch of buffers in the native rayon thread pool, off the JS
 * main thread. `tasks[i]` describes how to compress `buffers[i]`.
 */
export const compressBuffers = native.compressBuffers

export type { BatchOptions, CompressResult, CompressTask }
