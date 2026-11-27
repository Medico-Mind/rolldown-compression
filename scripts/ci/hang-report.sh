#!/usr/bin/env bash
# Run a Node command with a deadline and, if it is still running when the
# deadline passes, dump where it is stuck before killing it.
#
#   scripts/ci/hang-report.sh <seconds> <command> [args...]
#
# Exits with the command's own status when it finishes in time, 124 on a hang.
#
# While it runs, trace-blocking.cjs (preloaded) writes every JS Atomics.wait,
# Worker creation and binding module load synchronously to stderr, so the
# last trace lines name the call that never returned.
#
# On a hang it then collects, in order:
#   1. every thread of the process with its kernel wait channel (Linux),
#   2. a Node diagnostic report (JS stack, native stack, libuv handles and the
#      same for each Worker), triggered through SIGUSR2. The isolate has to
#      take an interrupt to write it, so a thread parked in Atomics.wait
#      writes none; a thread spinning in JS or wasm does,
#   3. native backtraces of every thread through gdb (Linux, when sudo can
#      install it), which work whatever state the threads are in: a frame in
#      FutexEmulation::Wait is an Atomics.wait, epoll/pthread frames are libuv.
# The report files stay in $NODE_REPORT_DIR for an artifact upload.
set -uo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <seconds> <command> [args...]" >&2
  exit 2
fi

deadline=$1
shift

report_dir=${NODE_REPORT_DIR:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/node-report}
mkdir -p "$report_dir"

trace_preload="$(cd "$(dirname "$0")" && pwd)/trace-blocking.cjs"

# NODE_OPTIONS reaches the child even when the command is not `node` itself
# (npm, npx), and Workers inherit it.
export NODE_OPTIONS="${NODE_OPTIONS:+$NODE_OPTIONS }--require=$trace_preload --report-on-signal --report-signal=SIGUSR2 --report-directory=$report_dir --report-exclude-network --trace-warnings"

"$@" &
pid=$!

for _ in $(seq 1 "$deadline"); do
  if ! kill -0 "$pid" 2>/dev/null; then
    wait "$pid"
    exit $?
  fi
  sleep 1
done

echo "::error::'$*' still running after ${deadline}s; collecting hang diagnostics"

echo "::group::Threads and kernel wait channels"
if [ -d "/proc/$pid/task" ]; then
  ps -L -o tid,stat,pcpu,wchan:32,comm -p "$pid" || true
else
  ps -o pid,stat,pcpu,command -p "$pid" || true
fi
echo "::endgroup::"

echo "::group::Node diagnostic report"
kill -USR2 "$pid" 2>/dev/null || true
for _ in $(seq 1 20); do
  compgen -G "$report_dir/report.*.json" >/dev/null && break
  sleep 1
done
# Give a report that is still being written a moment to finish.
sleep 2
report_found=false
for report in "$report_dir"/report.*.json; do
  [ -e "$report" ] || continue
  report_found=true
  echo "--- $report"
  node -e '
    const report = JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8"))
    const show = (r, prefix) => {
      const js = r.javascriptStack || {}
      console.log(`${prefix}thread ${r.header?.threadId ?? "?"} JS: ${js.message ?? "(none)"}`)
      for (const line of js.stack || []) console.log(`${prefix}  ${line}`)
      console.log(`${prefix}native:`)
      for (const frame of (r.nativeStack || []).slice(0, 40)) console.log(`${prefix}  ${frame.pc} ${frame.symbol}`)
      const active = (r.libuv || []).filter((h) => h.is_active)
      console.log(`${prefix}active libuv handles: ${active.map((h) => h.type).join(", ") || "(none)"}`)
      for (const [index, worker] of (r.workers || []).entries()) show(worker, `${prefix}[worker ${index}] `)
    }
    show(report, "")
  ' "$report" || cat "$report"
done
$report_found || echo "no report written: the isolate never took the SIGUSR2 interrupt"
echo "::endgroup::"

if [ -d "/proc/$pid/task" ] && command -v sudo >/dev/null; then
  echo "::group::Native backtraces (gdb)"
  if ! command -v gdb >/dev/null; then
    sudo apt-get update -qq >/dev/null 2>&1 || true
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq gdb >/dev/null 2>&1 || true
  fi
  if command -v gdb >/dev/null; then
    sudo timeout 120 gdb -batch -nx -p "$pid" \
      -ex "set pagination off" \
      -ex "info threads" \
      -ex "thread apply all bt 40" 2>&1 | grep -v '^\[New LWP' | tail -n 1500 || true
  else
    echo "gdb unavailable"
  fi
  echo "::endgroup::"
fi

kill -9 "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
exit 124
