import test from 'node:test'
import assert from 'node:assert/strict'

import {
  buildPinnedImageRef,
  extractBuildxDigest,
  renderUmbrelCompose,
  renderUmbrelManifest,
  validatePinnedImageRef,
} from './umbrel-release.mjs'

test('buildPinnedImageRef renders a pinned tag plus digest', () => {
  const digest = `sha256:${'a'.repeat(64)}`
  assert.equal(
    buildPinnedImageRef('ghcr.io/example/nostr-vpn-umbrel', '0.3.4', digest),
    `ghcr.io/example/nostr-vpn-umbrel:v0.3.4@${digest}`,
  )
})

test('validatePinnedImageRef rejects unpinned refs', () => {
  assert.throws(
    () => validatePinnedImageRef('ghcr.io/example/nostr-vpn-umbrel:v0.3.4'),
    /Expected a pinned image reference/,
  )
})

test('extractBuildxDigest reads the primary metadata field', () => {
  const digest = `sha256:${'b'.repeat(64)}`
  const metadata = JSON.stringify({
    'containerimage.digest': digest,
    'containerimage.descriptor': {
      digest,
    },
  })

  assert.equal(extractBuildxDigest(metadata), digest)
})

test('renderUmbrelCompose includes the pinned image and tunnel access', () => {
  const digest = `sha256:${'c'.repeat(64)}`
  const compose = renderUmbrelCompose(
    `ghcr.io/example/nostr-vpn-umbrel:v0.3.4@${digest}`,
  )

  assert.match(compose, /image: ghcr\.io\/example\/nostr-vpn-umbrel:v0\.3\.4@sha256:c+/)
  assert.match(compose, /network_mode: "host"/)
  assert.match(compose, /\/dev\/net\/tun:\/dev\/net\/tun/)
})

test('renderUmbrelManifest syncs version and release notes', () => {
  const manifest = renderUmbrelManifest(
    `manifestVersion: 1
version: "v0.3.4"
releaseNotes: ""
`,
    {
      tag: '0.3.5',
      releaseNotes: 'https://example.test/releases/v0.3.5',
    },
  )

  assert.match(manifest, /^version: "v0\.3\.5"$/m)
  assert.match(manifest, /^releaseNotes: "https:\/\/example\.test\/releases\/v0\.3\.5"$/m)
})
