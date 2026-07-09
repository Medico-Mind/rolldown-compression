/**
 * Pins the root package's optionalDependencies (the npm/* platform binary
 * packages) to the root package version.
 *
 * `napi version` bumps the npm/* packages themselves but never the root
 * optionalDependencies, and the release workflow publishes with
 * --ignore-scripts, skipping `napi prepublish` (the napi-rs step that would
 * otherwise write them). 0.3.1 shipped without any optionalDependencies for
 * exactly that reason, so version-packages runs this script explicitly.
 *
 * Usage:
 *   node scripts/sync-optional-deps.mjs          rewrite package.json
 *   node scripts/sync-optional-deps.mjs --check  exit 1 if out of sync
 */
import { readdirSync, readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(fileURLToPath(import.meta.url), '../..')
const packageJsonPath = path.join(root, 'package.json')
const packageJson = JSON.parse(readFileSync(packageJsonPath, 'utf-8'))

const expected = Object.fromEntries(
  readdirSync(path.join(root, 'npm'))
    .map((dir) => path.join(root, 'npm', dir, 'package.json'))
    .map((file) => [JSON.parse(readFileSync(file, 'utf-8')).name, packageJson.version])
    .sort(([a], [b]) => a.localeCompare(b)),
)

const actual = packageJson.optionalDependencies ?? {}
const inSync =
  Object.keys(expected).length === Object.keys(actual).length &&
  Object.entries(expected).every(([name, version]) => actual[name] === version)

if (inSync) {
  console.log(
    `optionalDependencies in sync (${Object.keys(expected).length} platform packages @ ${packageJson.version})`,
  )
  process.exit(0)
}

if (process.argv.includes('--check')) {
  console.error('optionalDependencies out of sync with npm/* platform packages:')
  console.error(`  expected: ${JSON.stringify(expected, null, 2)}`)
  console.error(`  actual:   ${JSON.stringify(actual, null, 2)}`)
  console.error(
    'run `node scripts/sync-optional-deps.mjs` (part of `npm run version-packages`) to fix',
  )
  process.exit(1)
}

packageJson.optionalDependencies = expected
writeFileSync(packageJsonPath, `${JSON.stringify(packageJson, null, 2)}\n`)
console.log(
  `optionalDependencies updated (${Object.keys(expected).length} platform packages @ ${packageJson.version})`,
)
