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
#      same for each Worker), triggered through SIGUSR2. It is written from
#      the event loop, so a thread that blocks or spins writes none,
#   3. through gdb (Linux, when sudo can install it): a few samples of the
#      main thread's JIT/wasm stack, walked along the frame pointer chain and
#      named through V8's perf map (--perf-basic-prof), so a busy loop in wasm
#      shows its Rust function; then native backtraces of every thread, where
#      FutexEmulation::Wait is an Atomics.wait and epoll/pthread is libuv.
# The report files and the perf map stay in $NODE_REPORT_DIR for an artifact
# upload.
set -uo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <seconds> <command> [args...]" >&2
  exit 2
fi

deadline=$1
shift

report_dir=${NODE_REPORT_DIR:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/node-report}
mkdir -p "$report_dir"

script_dir="$(cd "$(dirname "$0")" && pwd)"

# NODE_OPTIONS reaches the child even when the command is not `node` itself
# (npm, npx), and Workers inherit it.
export NODE_OPTIONS="${NODE_OPTIONS:+$NODE_OPTIONS }--require=$script_dir/trace-blocking.cjs --perf-basic-prof --report-on-signal --report-signal=SIGUSR2 --report-directory=$report_dir --report-exclude-network --trace-warnings"

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
for _ in $(seq 1 5); do
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
    perf_map="/tmp/perf-$pid.map"
    [ -f "$perf_map" ] && cp "$perf_map" "$report_dir/" || true
    # x86-64 only: V8 JIT and wasm frames keep rbp as a frame pointer, which
    # gdb has no unwind info to follow by itself.
    walk="$report_dir/walk-frames.gdb"
    cat > "$walk" <<'GDB'
set pagination off
thread 1
printf "pc  %#lx\n", $pc
set $fp = $rbp
set $depth = 0
while $depth < 64 && $fp != 0
  printf "ret %#lx\n", *(unsigned long *)($fp + 8)
  set $fp = *(unsigned long *)$fp
  set $depth = $depth + 1
end
GDB
    echo "::endgroup::"
    for sample in 1 2 3 4 5; do
      echo "::group::Main thread JIT/wasm stack, sample $sample"
      sudo timeout 60 gdb -batch -nx -p "$pid" -x "$walk" 2>&1 \
        | grep -E '^(pc|ret) ' \
        | if [ -f "$perf_map" ]; then node "$script_dir/symbolize-perf-map.cjs" "$perf_map"; else cat; fi \
        || true
      echo "::endgroup::"
      sleep 1
    done
    echo "::group::Native backtraces (gdb)"
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
