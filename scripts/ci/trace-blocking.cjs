// Preloaded by hang-report.sh (`--require`) to show where a process blocks
// for good. A main thread parked in Atomics.wait never takes the interrupt a
// diagnostic report needs, and the warnings it already emitted are still
// queued for the next tick, so nothing reaches the log. Everything here is
// written with fs.writeSync before the blocking call is made, so the last
// lines in the log name the call that never returned.
//
// Traced, on every thread this preload reaches:
//   - each JS-side Atomics.wait (wasm `memory.atomic.wait` instructions do not
//     go through it), with its stack for the first calls,
//   - each Worker created, with its stack,
//   - each module loaded from the binding, napi-rs or emnapi, so the last one
//     shows how far the loader got.
'use strict'

const fs = require('node:fs')
const Module = require('node:module')
const { isMainThread, threadId, Worker } = require('node:worker_threads')
const workerThreads = require('node:worker_threads')

const start = process.hrtime.bigint()
const tag = isMainThread ? 'main' : `worker ${threadId}`

function log(message) {
  const ms = Number(process.hrtime.bigint() - start) / 1e6
  try {
    fs.writeSync(2, `[trace +${ms.toFixed(1)}ms ${tag}] ${message}\n`)
  } catch {}
}

function stack(skip) {
  return (new Error().stack || '')
    .split('\n')
    .slice(skip)
    .map((line) => `    ${line.trim()}`)
    .join('\n')
}

const STACKED_WAITS = 25
let waits = 0
const originalWait = Atomics.wait
Atomics.wait = function wait(typedArray, index, value, timeout) {
  waits += 1
  if (waits <= STACKED_WAITS) {
    log(`Atomics.wait #${waits} index=${index} value=${value} timeout=${timeout}\n${stack(2)}`)
  } else if (waits % 1000 === 0) {
    log(`Atomics.wait #${waits}`)
  }
  const result = originalWait.call(this, typedArray, index, value, timeout)
  if (waits <= STACKED_WAITS) log(`Atomics.wait #${waits} -> ${result}`)
  return result
}

let workers = 0
class TracedWorker extends Worker {
  constructor(filename, options) {
    workers += 1
    const id = workers
    log(`new Worker #${id} ${String(filename)}\n${stack(2)}`)
    super(filename, options)
    log(`new Worker #${id} -> threadId ${this.threadId}`)
    this.once('online', () => log(`Worker threadId ${this.threadId} online`))
    this.once('exit', (code) => log(`Worker threadId ${this.threadId} exit ${code}`))
  }
}
workerThreads.Worker = TracedWorker

const traced = /rolldown-compression|wasi-worker|@napi-rs|@emnapi|emnapi|wasm-smoke/
const originalLoad = Module._load
Module._load = function load(request, parent, ...rest) {
  const from = parent?.filename
  const watch = traced.test(request) || (from && traced.test(from) && request.startsWith('.'))
  if (watch) log(`require ${request}${from ? ` from ${from}` : ''}`)
  const result = originalLoad.call(this, request, parent, ...rest)
  if (watch) log(`require ${request} done`)
  return result
}

log(`trace-blocking loaded (pid ${process.pid})`)
