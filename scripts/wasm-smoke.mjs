/**
 * Loads the binding through the generated index.js loader and runs one real
 * compression batch across all three algorithms. Run with
 * NAPI_RS_FORCE_WASI=error to prove the wasm binding actually works: 'error'
 * makes the loader throw instead of silently falling back to a native .node,
 * and the batch exercises the threaded (rayon) path that is the risky part
 * of the wasm build.
 */
import assert from 'node:assert'
import { createRequire } from 'node:module'

const { compressBuffers } = createRequire(import.meta.url)('../index.js')

const input = Buffer.from('export const value = 42;\n'.repeat(500))
const tasks = ['gzip', 'brotli', 'zstd'].map((algorithm) => ({
  fileName: `smoke.js.${algorithm}`,
  algorithm,
}))

const results = await compressBuffers(
  tasks,
  tasks.map(() => input),
)

for (const result of results) {
  assert.ok(!result.error, `${result.algorithm} failed: ${result.error}`)
  assert.ok(result.compressedSize > 0, `${result.algorithm} produced no output`)
  assert.ok(result.compressedSize < input.length, `${result.algorithm} did not shrink the input`)
  console.log(`${result.algorithm}: ${result.originalSize} -> ${result.compressedSize} bytes`)
}
console.log('smoke ok')
