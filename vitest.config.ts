import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    include: ['__tests__/**/*.test.ts'],
    testTimeout: 120_000,
    hookTimeout: 120_000,
    coverage: {
      provider: 'v8',
      include: ['ts/**/*.ts'],
      thresholds: {
        lines: 90,
      },
      reporter: ['text', 'lcov'],
    },
  },
})
