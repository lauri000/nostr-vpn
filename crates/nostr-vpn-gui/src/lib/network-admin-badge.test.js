import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const appSource = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8')

test('active network header shows a dedicated admin badge for local admins', () => {
  assert.match(
    appSource,
    /\{#if activeNetworkView\.localIsAdmin\}\s*<span class="badge ok" data-testid="active-network-admin-badge">\s*Admin\s*<\/span>/s,
  )
})

test('saved network cards show a dedicated admin badge for local admins', () => {
  assert.match(
    appSource,
    /\{#if network\.localIsAdmin\}\s*<span class="badge ok" data-testid="saved-network-admin-badge">\s*Admin\s*<\/span>/s,
  )
})
