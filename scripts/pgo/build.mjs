/**
 * PGO (+ optional BOLT) release build pipeline.
 *
 * Steps:
 *   1. baseline release build            -> target/pgo/baseline.node
 *   2. instrumented build (-Cprofile-generate)
 *   3. training run (scripts/pgo/train.mjs) -> .profraw profiles
 *   4. llvm-profdata merge
 *   5. optimized build (-Cprofile-use)   -> target/pgo/pgo.node (+ repo root)
 *   6. BOLT post-link optimization       -> target/pgo/bolt.node (+ repo root)
 *      Linux ELF only: llvm-bolt does not support Mach-O or PE, so this step
 *      is skipped on macOS/Windows. Requires llvm-bolt and merge-fdata on
 *      PATH (apt/dnf: llvm-bolt, or a full LLVM distribution).
 *
 * The final platform binding in the repo root is the most-optimized variant,
 * so `npm run bench` / `npm test` pick it up automatically. Compare variants
 * with `npm run bench:pgo`.
 *
 * Usage: node scripts/pgo/build.mjs [options]
 *   --no-bolt           skip the BOLT pass even on Linux
 *   --skip-baseline     skip the baseline build (CI: only the PGO binary matters)
 *   --napi-args "..."   extra args for `napi build` (e.g. "--target <triple> --use-napi-cross")
 *   --train "..."       shell command that runs the training workload against the
 *                       instrumented binding; `{binding}` is replaced with its
 *                       repo-relative path. Used in CI for cross-compiled targets
 *                       that must train under Docker/QEMU or Rosetta.
 *                       Default: node scripts/pgo/train.mjs {binding}
 */
import { execFileSync, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..')
const pgoDir = path.join(root, 'target', 'pgo')
const profilesDir = path.join(pgoDir, 'profiles')
const mergedProfile = path.join(pgoDir, 'merged.profdata')

const flags = new Set()
let napiArgs = []
let trainCommand = null
const argv = process.argv.slice(2)
for (let i = 0; i < argv.length; i++) {
  const arg = argv[i]
  if (arg.startsWith('--napi-args=')) napiArgs = arg.slice('--napi-args='.length).split(' ')
  else if (arg === '--napi-args') napiArgs = (argv[++i] ?? '').split(' ')
  else if (arg.startsWith('--train=')) trainCommand = arg.slice('--train='.length)
  else if (arg === '--train') trainCommand = argv[++i]
  else flags.add(arg)
}
napiArgs = napiArgs.filter(Boolean)
const noBolt = flags.has('--no-bolt')
const skipBaseline = flags.has('--skip-baseline')

const baseRustflags = process.env.RUSTFLAGS ?? ''

function log(message) {
  console.log(`\n=== ${message} ===`)
}

function run(command, args, extraEnv = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: 'inherit',
    // npx and friends are .cmd shims on Windows and need a shell to launch.
    shell: process.platform === 'win32',
    env: { ...process.env, ...extraEnv },
  })
  if (result.error) throw result.error
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} exited with ${result.status}`)
  }
}

function napiBuild(rustflags, extraEnv = {}) {
  run('npx', ['--no-install', 'napi', 'build', '--platform', '--release', ...napiArgs], {
    RUSTFLAGS: `${baseRustflags} ${rustflags}`.trim(),
    ...extraEnv,
  })
  return findPlatformBinding()
}

/** The build output: rolldown-compression.<platform>.node in the repo root. */
function findPlatformBinding() {
  const candidates = fs
    .readdirSync(root)
    .filter((name) => name.startsWith('rolldown-compression.') && name.endsWith('.node'))
    .map((name) => path.join(root, name))
    .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)
  if (candidates.length === 0) {
    throw new Error('no rolldown-compression.*.node produced by napi build')
  }
  return candidates[0]
}

/**
 * Prefer the llvm tools bundled with the active rustc (same LLVM version as
 * the profile format), falling back to PATH.
 */
function findRustcLlvmTool(name) {
  try {
    const sysroot = execFileSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' }).trim()
    const host = execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
      .split('\n')
      .find((line) => line.startsWith('host: '))
      ?.slice('host: '.length)
    const executable = process.platform === 'win32' ? `${name}.exe` : name
    const bundled = path.join(sysroot, 'lib', 'rustlib', host ?? '', 'bin', executable)
    if (fs.existsSync(bundled)) return bundled
  } catch {
    // fall through to PATH lookup
  }
  return findOnPath(name)
}

function findOnPath(name) {
  const probe = spawnSync(name, ['--version'], { stdio: 'ignore' })
  return probe.error ? null : name
}

function train(bindingPath, extraEnv = {}) {
  // Repo-relative path: a --train docker command that mounts the workspace at
  // the same path (-v $PWD:$PWD -w $PWD) resolves it unchanged.
  const relative = path.relative(root, bindingPath)
  if (trainCommand) {
    const command = trainCommand.replaceAll('{binding}', relative)
    const result = spawnSync(command, {
      cwd: root,
      stdio: 'inherit',
      shell: true,
      env: { ...process.env, ...extraEnv },
    })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`train command exited with ${result.status}`)
    return
  }
  run('node', [path.join('scripts', 'pgo', 'train.mjs'), relative], extraEnv)
}

const llvmProfdata = findRustcLlvmTool('llvm-profdata')
if (!llvmProfdata) {
  console.error(
    'llvm-profdata not found. Install it with `rustup component add llvm-tools` (or add an LLVM distribution to PATH).',
  )
  process.exit(1)
}

fs.rmSync(profilesDir, { recursive: true, force: true })
fs.mkdirSync(profilesDir, { recursive: true })

if (skipBaseline) {
  log('step 1/5: baseline build (skipped)')
} else {
  log('step 1/5: baseline release build')
  const baseline = napiBuild('')
  fs.copyFileSync(baseline, path.join(pgoDir, 'baseline.node'))
}

log('step 2/5: instrumented build (-Cprofile-generate)')
const instrumented = napiBuild(`-Cprofile-generate=${profilesDir}`)
fs.copyFileSync(instrumented, path.join(pgoDir, 'instrumented.node'))

log('step 3/5: training run')
train(path.join(pgoDir, 'instrumented.node'), {
  LLVM_PROFILE_FILE: path.join(profilesDir, 'train-%p-%m.profraw'),
})

log('step 4/5: merging profiles')
run(llvmProfdata, ['merge', '-o', mergedProfile, profilesDir])

log('step 5/5: optimized build (-Cprofile-use)')
const pgoFlags = `-Cprofile-use=${mergedProfile}`
// BOLT rewrites the ELF layout post-link: it needs relocations preserved and
// symbols intact, so the BOLT-input build disables `strip = "symbols"`.
const wantBolt = !noBolt && process.platform === 'linux'
const boltTool = wantBolt ? findOnPath('llvm-bolt') : null
const mergeFdata = wantBolt ? findOnPath('merge-fdata') : null
const runBolt = wantBolt && boltTool && mergeFdata

const optimized = runBolt
  ? napiBuild(`${pgoFlags} -Clink-arg=-Wl,--emit-relocs`, { CARGO_PROFILE_RELEASE_STRIP: 'none' })
  : napiBuild(pgoFlags)
fs.copyFileSync(optimized, path.join(pgoDir, 'pgo.node'))

if (runBolt) {
  log('BOLT: instrumenting')
  const boltDir = path.join(pgoDir, 'bolt-profiles')
  fs.rmSync(boltDir, { recursive: true, force: true })
  fs.mkdirSync(boltDir, { recursive: true })
  const boltInstrumented = path.join(pgoDir, 'bolt-instrumented.node')
  run(boltTool, [
    path.join(pgoDir, 'pgo.node'),
    '-instrument',
    '-o',
    boltInstrumented,
    `--instrumentation-file=${path.join(boltDir, 'train')}`,
    '--instrumentation-file-append-pid',
  ])

  log('BOLT: training run')
  train(boltInstrumented)

  log('BOLT: merging profiles and optimizing')
  const fdataFiles = fs
    .readdirSync(boltDir)
    .filter((name) => name.startsWith('train'))
    .map((name) => path.join(boltDir, name))
  if (fdataFiles.length === 0) throw new Error('BOLT training produced no .fdata profiles')
  const mergedFdata = path.join(pgoDir, 'merged.fdata')
  fs.writeFileSync(
    mergedFdata,
    execFileSync(mergeFdata, fdataFiles, { maxBuffer: 1024 * 1024 * 1024 }),
  )

  const bolted = path.join(pgoDir, 'bolt.node')
  run(boltTool, [
    path.join(pgoDir, 'pgo.node'),
    '-o',
    bolted,
    `-data=${mergedFdata}`,
    '-reorder-blocks=ext-tsp',
    '-reorder-functions=cdsort',
    '-split-functions',
    '-split-all-cold',
    '-split-eh',
    '-icf=1',
    '-dyno-stats',
  ])
  // Ship the fully optimized binding as the platform binary.
  fs.copyFileSync(bolted, optimized)
} else if (wantBolt) {
  console.log(
    '\nBOLT skipped: llvm-bolt and/or merge-fdata not found on PATH (install the llvm-bolt package).',
  )
} else if (!noBolt) {
  console.log(`\nBOLT skipped: unsupported platform ${process.platform} (BOLT requires Linux ELF).`)
}

log('done')
console.log(`artifacts in ${path.relative(root, pgoDir)}/:`)
for (const name of ['baseline.node', 'pgo.node', 'bolt.node']) {
  const file = path.join(pgoDir, name)
  if (fs.existsSync(file)) {
    console.log(`  ${name}: ${(fs.statSync(file).size / 1024).toFixed(0)} KB`)
  }
}
console.log(`platform binding (${path.basename(optimized)}) is the optimized build`)
console.log('compare with: npm run bench:pgo')
