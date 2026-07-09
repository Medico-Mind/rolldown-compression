import { defineConfig } from 'tsdown'

export default defineConfig({
  entry: ['ts/index.ts'],
  format: ['esm', 'cjs'],
  outDir: 'dist',
  dts: true,
  shims: true,
  fixedExtension: true,
})
