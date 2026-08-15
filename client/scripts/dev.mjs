#!/usr/bin/env node
/**
 * Dev-mode server for `tauri dev`.
 *
 * Ensures the dsh kernel is installed, then serves `dsh web` on the fixed
 * dev port 1420 (matching devUrl in tauri.conf.json) using the system Node.
 * Forwards child output to our stdout so `tauri dev` shows dsh logs, and
 * kills the child when we are terminated.
 */

import { spawn } from 'node:child_process'
import { execSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const CLIENT = path.resolve(__dirname, '..')
const RES = path.join(CLIENT, 'resources')

const DEV_PORT = 1420
const DSH_VERSION = process.env.DSH_VERSION || '0.1.0-rc.6'

function ensureKernel() {
  const marker = path.join(RES, 'kernel.version')
  const tarball = path.join(RES, 'kernel.tar.gz')
  if (existsSync(tarball) && existsSync(marker) && readFileSync(marker, 'utf8').trim() === DSH_VERSION) return
  console.log('[dev] kernel missing, preparing…')
  execSync(`node ${path.join(__dirname, 'prepare.mjs')}`, { stdio: 'inherit' })
}

function dshEntry() {
  const dir = path.join(RES, 'kernel-dev', 'node_modules', '@deepseek-ai', 'dsh')
  const entry = path.join(dir, 'lib', 'bin.js')
  if (!existsSync(entry)) {
    mkdirSync(path.join(RES, 'kernel-dev'), { recursive: true })
    execSync(
      `npm install --prefix ${JSON.stringify(path.join(RES, 'kernel-dev'))} --omit=dev --no-audit --no-fund @deepseek-ai/dsh@${DSH_VERSION}`,
      { stdio: 'inherit' },
    )
  }
  return entry
}

function main() {
  ensureKernel()
  const entry = dshEntry()
  const child = spawn(process.execPath, [entry, '--profile', 'web', '--host', '127.0.0.1', '--port', String(DEV_PORT)], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, DSH_HOME: process.env.DSH_HOME || undefined },
  })
  let ready = false
  const onChunk = (buf, stream) => {
    process[stream].write(buf)
    if (!ready && buf.toString().includes(`127.0.0.1:${DEV_PORT}`)) {
      ready = true
      console.log(`[dev] dsh web ready on http://127.0.0.1:${DEV_PORT}`)
    }
  }
  child.stdout.on('data', (b) => onChunk(b, 'stdout'))
  child.stderr.on('data', (b) => onChunk(b, 'stderr'))
  child.on('exit', (code, signal) => {
    console.log(`[dev] dsh exited (${code ?? signal})`)
    process.exit(0)
  })
  const shutdown = () => {
    try {
      child.kill('SIGTERM')
    } catch {
      /* already gone */
    }
  }
  process.on('SIGINT', shutdown)
  process.on('SIGTERM', shutdown)
}

main()
