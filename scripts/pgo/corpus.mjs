/**
 * Static training corpus for PGO / BOLT profiling.
 *
 * Generates deterministic variations of the content a Rolldown dist/ folder
 * actually contains — JS bundles, JSON, CSS, HTML, source maps, base64 asset
 * blobs — plus edge-case payloads (incompressible noise, all-zero runs, empty
 * and tiny buffers, multi-byte unicode) so every branch of the encoders gets
 * profile coverage, not just the happy path.
 *
 * All content is derived from a fixed xorshift seed: two runs of the trainer
 * produce byte-identical inputs, which keeps PGO profiles reproducible.
 */

function makeRand(seed) {
  let state = seed >>> 0
  return () => {
    state ^= state << 13
    state ^= state >>> 17
    state ^= state << 5
    return (state >>> 0) / 0xffffffff
  }
}

const WORDS =
  'component render props state effect memo callback context reducer selector dispatch module export import default async await promise buffer stream router query mutation schema resolver'.split(
    ' ',
  )

const pick = (rand, list) => list[Math.floor(rand() * list.length)]

function jsModule(rand, target) {
  const lines = []
  let size = 0
  let index = 0
  while (size < target) {
    const a = pick(rand, WORDS)
    const b = pick(rand, WORDS)
    const line = `export function ${a}_${b}_${index++}(input) { return input.${a}?.${b} ?? ${Math.floor(rand() * 1e6)}; }\n`
    lines.push(line)
    size += line.length
  }
  return Buffer.from(lines.join(''))
}

function json(rand, target) {
  const entries = []
  let size = 0
  let index = 0
  while (size < target) {
    const entry = JSON.stringify({
      id: index++,
      name: `${pick(rand, WORDS)}-${pick(rand, WORDS)}`,
      enabled: rand() > 0.5,
      weight: Math.floor(rand() * 1e6) / 1000,
      tags: [pick(rand, WORDS), pick(rand, WORDS), pick(rand, WORDS)],
    })
    entries.push(entry)
    size += entry.length + 1
  }
  return Buffer.from(`[${entries.join(',\n')}]`)
}

function css(rand, target) {
  const rules = []
  let size = 0
  let index = 0
  while (size < target) {
    const rule = `.${pick(rand, WORDS)}-${index++}{display:flex;margin:${Math.floor(rand() * 64)}px;padding:${Math.floor(rand() * 32)}px;color:#${Math.floor(
      rand() * 0xffffff,
    )
      .toString(16)
      .padStart(6, '0')};flex-direction:${rand() > 0.5 ? 'row' : 'column'}}\n`
    rules.push(rule)
    size += rule.length
  }
  return Buffer.from(rules.join(''))
}

function html(rand, target) {
  const parts = ['<!doctype html><html><head><meta charset="utf-8"></head><body>']
  let size = parts[0].length
  let index = 0
  while (size < target) {
    const part = `<section id="s${index++}" class="${pick(rand, WORDS)} ${pick(rand, WORDS)}"><h2>${pick(rand, WORDS)} ${pick(rand, WORDS)}</h2><p>${pick(rand, WORDS)} ${pick(rand, WORDS)} ${pick(rand, WORDS)} ${Math.floor(rand() * 1e4)}</p></section>\n`
    parts.push(part)
    size += part.length
  }
  parts.push('</body></html>')
  return Buffer.from(parts.join(''))
}

function sourceMap(rand, target) {
  // VLQ-shaped mappings string: long runs of a small ASCII alphabet with
  // frequent separators, a distinct entropy profile from JS or JSON.
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
  const chunks = []
  let size = 0
  while (size < target) {
    const run = Array.from({ length: 4 + Math.floor(rand() * 8) }, () => pick(rand, alphabet)).join(
      '',
    )
    const chunk = rand() > 0.85 ? `;${run}` : `,${run}`
    chunks.push(chunk)
    size += chunk.length
  }
  return Buffer.from(`{"version":3,"mappings":"${chunks.join('')}"}`)
}

function base64Blob(rand, target) {
  const raw = Buffer.alloc(Math.ceil((target * 3) / 4))
  for (let i = 0; i < raw.length; i++) raw[i] = Math.floor(rand() * 256)
  return Buffer.from(`data:font/woff2;base64,${raw.toString('base64')}`)
}

function unicodeText(rand, target) {
  const samples = [
    'Сжатие данных без потерь на лету',
    '高速なデータ圧縮アルゴリズム',
    'Ταχεία συμπίεση δεδομένων',
    '🚀 emoji-heavy changelog entry ✨🎉',
    'Compression de données à la volée',
  ]
  const lines = []
  let size = 0
  while (size < target) {
    const line = `${pick(rand, samples)} — ${Math.floor(rand() * 1e5)}\n`
    lines.push(line)
    size += Buffer.byteLength(line)
  }
  return Buffer.from(lines.join(''))
}

function randomBinary(rand, target) {
  const buffer = Buffer.alloc(target)
  for (let i = 0; i < target; i++) buffer[i] = Math.floor(rand() * 256)
  return buffer
}

const KB = 1024
const MB = 1024 * KB

/**
 * Build the full corpus. Every file carries a `compressible` hint so the
 * trainer can route incompressible payloads through the skip path.
 */
export function makeCorpus() {
  const rand = makeRand(0x2545f491)
  const files = []
  const add = (name, data, compressible = true) => files.push({ name, data, compressible })

  const generators = [
    ['bundle.js', jsModule],
    ['manifest.json', json],
    ['styles.css', css],
    ['index.html', html],
    ['bundle.js.map', sourceMap],
    ['notes.txt', unicodeText],
  ]
  // Small route chunk / medium feature bundle / large-ish asset per type.
  const sizes = [
    ['sm', 8 * KB],
    ['md', 96 * KB],
    ['lg', 768 * KB],
  ]
  for (const [name, generate] of generators) {
    for (const [suffix, size] of sizes) {
      add(`${suffix}-${name}`, generate(rand, size))
    }
  }

  // Vendor-bundle-sized JS: the dominant cost in a real build.
  add('vendor.js', jsModule(rand, 2.5 * MB))

  // Base64 asset: text-encoded noise, compresses poorly but not never.
  add('font.woff2.css', base64Blob(rand, 192 * KB))

  // Incompressible / degenerate payloads.
  add('asset.bin', randomBinary(rand, 256 * KB), false)
  add('zeros.dat', Buffer.alloc(1 * MB))
  add('empty.txt', Buffer.alloc(0))
  add('tiny.txt', Buffer.from('x'.repeat(16)))

  return files
}
