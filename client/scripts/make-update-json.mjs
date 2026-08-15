#!/usr/bin/env node
/**
 * Generate the Tauri v2 update manifest (latest.json) from the assets of a
 * GitHub release, and write it to ./latest.json for upload.
 *
 * Env: GITHUB_TOKEN, REPO (owner/repo), TAG (release tag, e.g. v0.1.0)
 */

import { writeFileSync } from 'node:fs'

const { GITHUB_TOKEN, REPO, TAG } = process.env
if (!GITHUB_TOKEN || !REPO || !TAG) {
  console.error('usage: GITHUB_TOKEN=... REPO=owner/repo TAG=v0.1.0 node make-update-json.mjs')
  process.exit(1)
}

const API = 'https://api.github.com'
const headers = {
  Authorization: `Bearer ${GITHUB_TOKEN}`,
  Accept: 'application/vnd.github+json',
  'User-Agent': 'dsh-client-update-manifest',
}

const release = await fetch(`${API}/repos/${REPO}/releases/tags/${TAG}`, { headers }).then((r) => {
  if (!r.ok) throw new Error(`release lookup failed: ${r.status}`)
  return r.json()
})

/** Map a release asset name to its updater platform key. */
function platformKey(name) {
  if (name.endsWith('.app.tar.gz')) {
    if (name.includes('aarch64')) return 'darwin-aarch64'
    if (name.includes('x86_64')) return 'darwin-x86_64'
    return null
  }
  if (name.endsWith('.msi')) return 'windows-x86_64'
  if (name.endsWith('.AppImage')) {
    if (name.includes('aarch64')) return 'linux-aarch64'
    return 'linux-x86_64'
  }
  return null
}

const platforms = {}
for (const asset of release.assets) {
  const key = platformKey(asset.name)
  if (!key) continue
  const sigAsset = release.assets.find((s) => s.name === `${asset.name}.sig`)
  if (!sigAsset) {
    console.error(`missing signature for ${asset.name}`)
    continue
  }
  const signature = await fetch(sigAsset.browser_download_url, { headers }).then((r) => r.text())
  platforms[key] = { signature: signature.trim(), url: asset.browser_download_url }
}

if (Object.keys(platforms).length === 0) {
  console.error('no updater bundles found in release assets')
  process.exit(1)
}

const manifest = {
  version: TAG.replace(/^v/, ''),
  notes: (release.body || `DSH Client ${TAG}`).slice(0, 1000),
  pub_date: release.published_at || new Date().toISOString(),
  platforms,
}

writeFileSync('latest.json', JSON.stringify(manifest, null, 2))
console.log(`latest.json written: version ${manifest.version}, platforms: ${Object.keys(platforms).join(', ')}`)
