import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'

import {
  autoDetectWindowsVmName,
  buildReleaseManifest,
  describeAsset,
  parseEnvFile,
  readWorkspaceVersionTag,
  renderReleaseNotes,
  splitCsv,
} from './local-release-lib.mjs'

test('parseEnvFile reads basic dotenv syntax', () => {
  const parsed = parseEnvFile(`
# comment
NVPN_RELEASE_TREE=nostr-vpn-releases
NVPN_WINDOWS_VM_NAME="Windows 11"
NVPN_NOTE='line one'
INVALID KEY=nope
`)

  assert.deepEqual(parsed, {
    NVPN_RELEASE_TREE: 'nostr-vpn-releases',
    NVPN_WINDOWS_VM_NAME: 'Windows 11',
    NVPN_NOTE: 'line one',
  })
})

test('splitCsv trims and drops empties', () => {
  assert.deepEqual(splitCsv('verify, windows,android ,, macos'), [
    'verify',
    'windows',
    'android',
    'macos',
  ])
})

test('readWorkspaceVersionTag reads the workspace package version', () => {
  const tag = readWorkspaceVersionTag(`
[workspace]
members = []

[workspace.package]
version = "0.2.27"
`)

  assert.equal(tag, 'v0.2.27')
})

test('autoDetectWindowsVmName returns the only running Windows VM', () => {
  const name = autoDetectWindowsVmName(`
UUID                                    STATUS       IP_ADDR         NAME
{1e553d3b-024e-4799-adb0-92127659f5dd}  running      -               Windows 11
`)

  assert.equal(name, 'Windows 11')
})

test('autoDetectWindowsVmName returns null when multiple Windows VMs match', () => {
  const name = autoDetectWindowsVmName(`
UUID                                    STATUS       IP_ADDR         NAME
{1}  running      -               Windows 11
{2}  running      -               Windows ARM
`)

  assert.equal(name, null)
})

test('describeAsset maps release filenames to readable labels', () => {
  assert.equal(
    describeAsset('nostr-vpn-v0.2.27-windows-x64-setup.exe'),
    'Windows x64 installer',
  )
  assert.equal(
    describeAsset('nvpn-v0.2.27-aarch64-pc-windows-msvc.zip'),
    'Windows ARM64 CLI',
  )
})

test('buildReleaseManifest records staged assets with sizes', () => {
  const root = mkdtempSync(join(tmpdir(), 'nostr-vpn-release-test-'))
  const assetsDir = join(root, 'assets')
  mkdirSync(assetsDir)
  const installer = join(assetsDir, 'nostr-vpn-v0.2.27-windows-x64-setup.exe')
  const cliZip = join(assetsDir, 'nvpn-v0.2.27-x86_64-pc-windows-msvc.zip')
  writeFileSync(installer, 'installer')
  writeFileSync(cliZip, 'zip')

  const manifest = buildReleaseManifest({
    tag: 'v0.2.27',
    commit: 'abc123',
    createdAt: 1774523304,
    assetPaths: [installer, cliZip],
  })

  assert.equal(manifest.assets.length, 2)
  assert.equal(manifest.assets[0].name, 'nostr-vpn-v0.2.27-windows-x64-setup.exe')
  assert.equal(manifest.assets[1].name, 'nvpn-v0.2.27-x86_64-pc-windows-msvc.zip')
  assert.equal(manifest.assets[0].path, 'assets/nostr-vpn-v0.2.27-windows-x64-setup.exe')
})

test('renderReleaseNotes includes built and skipped sections', () => {
  const notes = renderReleaseNotes({
    tag: 'v0.2.27',
    commit: 'abc123',
    assetNames: [
      'nostr-vpn-v0.2.27-macos-arm64.zip',
      'nvpn-v0.2.27-x86_64-pc-windows-msvc.zip',
    ],
    builtLines: ['Built Windows x64 CLI inside a local Parallels VM.'],
    skippedLines: ['Linux musl CLI skipped because cross was unavailable.'],
  })

  assert.match(notes, /Windows x64 CLI/)
  assert.match(notes, /Built Windows x64 CLI inside a local Parallels VM\./)
  assert.match(notes, /Linux musl CLI skipped because cross was unavailable\./)
})
