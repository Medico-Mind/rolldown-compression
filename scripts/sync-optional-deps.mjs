/**
 * Injects the npm/* platform binary packages into the root package.json's
 * optionalDependencies, pinned at the root version. The release workflow
 * runs this right before `npm publish` so the published tarball declares
 * them (0.3.1 shipped without any, and the native binding was never
 * installed).
 *
 * They can't simply be checked into package.json: at release-PR time the
 * new version's platform packages aren't on the registry yet, so
 * `npm install --package-lock-only` silently drops the unresolvable
 * optional deps from the lockfile and every subsequent `npm ci` fails with
 * "package.json and package-lock.json are not in sync". They exist only in
 * the published tarball, injected here (the napi-rs equivalent,
 * `napi prepublish`, is skipped by the publish loop's --ignore-scripts).
 *
 * Usage:
 *   node scripts/sync-optional-deps.mjs          inject into package.json
 *   node scripts/sync-optional-deps.mjs --check  exit 1 if package.json has
 *                                                them checked in
 */
import { readdirSync, readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(fileURLToPath(import.meta.url), '../..')
const packageJsonPath = path.join(root, 'package.json')
const packageJson = JSON.parse(readFileSync(packageJsonPath, 'utf-8'))

if (process.argv.includes('--check')) {
  if (packageJson.optionalDependencies) {
    console.error('package.json must not declare optionalDependencies:')
    console.error(`  ${JSON.stringify(packageJson.optionalDependencies, null, 2)}`)
    console.error(
      'the platform packages are injected at publish time by this script; checking them in desyncs the lockfile on the next release PR and breaks `npm ci`',
    )
    process.exit(1)
  }
  console.log('ok: no optionalDependencies checked in (injected at publish time)')
  process.exit(0)
}

const optionalDependencies = Object.fromEntries(
  readdirSync(path.join(root, 'npm'))
    .map((dir) => path.join(root, 'npm', dir, 'package.json'))
    .map((file) => [JSON.parse(readFileSync(file, 'utf-8')).name, packageJson.version])
    .sort(([a], [b]) => a.localeCompare(b)),
)

// after devDependencies, purely for readable ordering in the tarball
const { devDependencies, ...rest } = packageJson
writeFileSync(
  packageJsonPath,
  `${JSON.stringify({ ...rest, optionalDependencies, devDependencies }, null, 2)}\n`,
)
console.log(
  `optionalDependencies injected (${Object.keys(optionalDependencies).length} platform packages @ ${packageJson.version})`,
)
