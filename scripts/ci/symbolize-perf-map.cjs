// Resolve JIT and wasm code addresses through the perf map V8 writes under
// --perf-basic-prof (/tmp/perf-<pid>.map: "<start> <size> <name>" in hex).
//
//   node symbolize-perf-map.cjs <perf map> < addresses
//
// Reads text on stdin and appends the code object to every 0x... address in
// it. gdb cannot unwind V8 frames on its own; hang-report.sh walks the frame
// pointer chain and pipes the return addresses through here. Wasm functions
// carry their Rust names when the module keeps its name section
// (CARGO_PROFILE_RELEASE_STRIP=none).
'use strict'

const fs = require('node:fs')

const entries = []
for (const line of fs.readFileSync(process.argv[2], 'utf8').split('\n')) {
  const match = /^([0-9a-f]+) ([0-9a-f]+) (.*)$/i.exec(line)
  if (!match) continue
  const start = BigInt(`0x${match[1]}`)
  entries.push({ start, end: start + BigInt(`0x${match[2]}`), name: match[3] })
}

// Code is logged as it is created and an address range can be reused after a
// tier-up or GC, so the latest entry covering an address wins.
function resolve(address) {
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i]
    if (address >= entry.start && address < entry.end) {
      return `${entry.name}+0x${(address - entry.start).toString(16)}`
    }
  }
  return null
}

const input = fs.readFileSync(0, 'utf8')
process.stdout.write(
  input.replace(/0x[0-9a-f]{6,16}/gi, (text) => {
    const name = resolve(BigInt(text))
    return name ? `${text} ${name}` : text
  }),
)
