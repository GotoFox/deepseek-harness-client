#!/usr/bin/env node
/**
 * Prepare client runtime resources for Tauri bundling.
 *
 * Produces, inside client/resources/ (gitignored):
 *   - node/            Node.js runtime extracted for the current platform
 *   - kernel.tar.gz    tarball of a self-contained `@deepseek-ai/dsh` install
 *                      (npm install output, so the client runs fully offline)
 *   - kernel.version   the dsh version the tarball was built from
 *
 * Env overrides:
 *   DSH_VERSION        dsh kernel version (default 0.1.0-rc.6)
 *   NODE_VERSION       Node.js version (default v22.19.0, satisfies dsh engines)
 *   NPM_REGISTRY       registry for the kernel install (default https://registry.npmjs.org)
 *   NODE_DIST_MIRROR   Node.js binary mirror (default https://nodejs.org/dist)
 */

import { execSync } from 'node:child_process'
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  renameSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { pipeline } from 'node:stream/promises'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const ROOT = path.resolve(__dirname, '..')
const RES = path.join(ROOT, 'resources')

const DSH_VERSION = process.env.DSH_VERSION || '0.1.0-rc.6'
const NODE_VERSION = process.env.NODE_VERSION || 'v22.19.0'
const NPM_REGISTRY = process.env.NPM_REGISTRY || 'https://registry.npmjs.org'
const NODE_DIST_MIRROR = process.env.NODE_DIST_MIRROR || 'https://nodejs.org/dist'
const FORCE = process.argv.includes('--force')

const PLATFORM = (() => {
  const os = { darwin: 'darwin', win32: 'win32', linux: 'linux' }[process.platform]
  const arch = process.arch === 'x64' ? 'x64' : process.arch === 'arm64' ? 'arm64' : process.arch
  if (!os) throw new Error(`unsupported platform: ${process.platform}`)
  if (!['x64', 'arm64'].includes(arch)) throw new Error(`unsupported arch: ${arch}`)
  return { os, arch }
})()

function log(msg) {
  console.log(`[prepare] ${msg}`)
}

async function download(url, dest) {
  log(`downloading ${url}`)
  mkdirSync(path.dirname(dest), { recursive: true })
  const res = await fetch(url)
  if (!res.ok) throw new Error(`download failed: ${res.status} ${res.statusText} for ${url}`)
  await pipeline(res.body, createWriteStream(dest))
}

function sh(cmd) {
  return execSync(cmd, { stdio: 'inherit' })
}

/** Recursively delete paths whose basename matches the predicate (no `find`). */
function deleteMatching(dir, pred) {
  if (!existsSync(dir)) return
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      if (pred(entry.name)) {
        rmSync(p, { recursive: true, force: true })
      } else {
        deleteMatching(p, pred)
      }
    } else if (pred(entry.name)) {
      rmSync(p, { force: true })
    }
  }
}

async function prepareNode() {
  const nodeDir = path.join(RES, 'node')
  const marker = path.join(RES, 'node.version')
  const want = `${PLATFORM.os}-${PLATFORM.arch} ${NODE_VERSION}`
  if (!FORCE && existsSync(marker) && readFileSync(marker, 'utf8').trim() === want) {
    log(`node runtime up to date (${want})`)
    return
  }
  rmSync(nodeDir, { recursive: true, force: true })
  mkdirSync(nodeDir, { recursive: true })
  const dist = `node-${NODE_VERSION}-${PLATFORM.os}-${PLATFORM.arch}`
  const ext = PLATFORM.os === 'win32' ? 'zip' : 'tar.gz'
  const archive = path.join(RES, `${dist}.${ext}`)
  await download(`${NODE_DIST_MIRROR}/${NODE_VERSION}/${dist}.${ext}`, archive)
  if (PLATFORM.os === 'win32') {
    // bsdtar on Windows handles zip archives.
    sh(`tar -xf "${archive}" -C "${RES}"`)
    renameSync(path.join(RES, dist, 'node.exe'), path.join(nodeDir, 'node.exe'))
  } else {
    sh(`tar -xzf "${archive}" -C "${RES}"`)
    renameSync(path.join(RES, dist, 'bin', 'node'), path.join(nodeDir, 'node'))
    execSync(`chmod +x "${path.join(nodeDir, 'node')}"`)
  }
  rmSync(path.join(RES, dist), { recursive: true, force: true })
  rmSync(archive, { force: true })
  writeFileSync(marker, want)
  log(`node runtime ready (${want})`)
}

async function prepareKernel() {
  const tarball = path.join(RES, 'kernel.tar.gz')
  const marker = path.join(RES, 'kernel.version')
  if (!FORCE && existsSync(tarball) && existsSync(marker) && readFileSync(marker, 'utf8').trim() === DSH_VERSION) {
    log(`kernel ${DSH_VERSION} already prepared`)
    return
  }
  const tmp = path.join(RES, '.kernel-build')
  rmSync(tmp, { recursive: true, force: true })
  mkdirSync(tmp, { recursive: true })
  writeFileSync(
    path.join(tmp, 'package.json'),
    JSON.stringify({ name: 'dsh-kernel', private: true, dependencies: { '@deepseek-ai/dsh': DSH_VERSION } }),
  )
  log(`installing @deepseek-ai/dsh@${DSH_VERSION} (this takes a while)`)
  sh(`npm install --prefix "${tmp}" --registry=${NPM_REGISTRY} --omit=dev --no-audit --no-fund`)
  // Drop prebuilt sandbox binaries for platforms we are not shipping.
  deleteMatching(path.join(tmp, 'node_modules'), (name) =>
    name.startsWith('prebuilds-') || (name.includes('linux-') || name.includes('darwin-')) && name !== `${PLATFORM.os}-${PLATFORM.arch}`,
  )
  log('packing kernel tarball')
  sh(`tar -czf "${tarball}" -C "${tmp}" node_modules`)
  writeFileSync(marker, DSH_VERSION)
  rmSync(tmp, { recursive: true, force: true })
  log(`kernel ${DSH_VERSION} ready (${Math.round(statSync(tarball).size / 1024 / 1024)} MB)`)
}

async function main() {
  mkdirSync(RES, { recursive: true })
  if (process.argv.includes('--bundle')) {
    await prepareNode()
  }
  await prepareKernel()
  log('resources ready')
}

main().catch((err) => {
  console.error(err)
  process.exit(1)
})
