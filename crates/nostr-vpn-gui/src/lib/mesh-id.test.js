import test from 'node:test'
import assert from 'node:assert/strict'

import {
  MESH_ID_COMPAT_PREFIX,
  canonicalizeMeshIdInput,
  formatMeshIdForDisplay,
  validateMeshIdInput,
} from './mesh-id.js'

test('formatMeshIdForDisplay strips the compat prefix and groups compact ids', () => {
  assert.equal(formatMeshIdForDisplay('nostr-vpn:1234abcd5678ef90'), '1234-abcd-5678-ef90')
  assert.equal(formatMeshIdForDisplay('mesh-home'), 'mesh-home')
})

test('canonicalizeMeshIdInput restores the hidden compat prefix for generated ids', () => {
  const currentMeshId = `${MESH_ID_COMPAT_PREFIX}1234abcd5678ef90`

  assert.equal(
    canonicalizeMeshIdInput('1234-abcd-5678-ef90', currentMeshId),
    `${MESH_ID_COMPAT_PREFIX}1234abcd5678ef90`,
  )
  assert.equal(
    canonicalizeMeshIdInput('mesh-home', currentMeshId),
    `${MESH_ID_COMPAT_PREFIX}meshhome`,
  )
})

test('validateMeshIdInput accepts legacy ids and rejects malformed grouped ids', () => {
  assert.equal(validateMeshIdInput('nostr-vpn'), '')
  assert.equal(validateMeshIdInput('abcd-efgh-ijkl'), '')
  assert.equal(validateMeshIdInput('mesh-home'), '')
  assert.equal(validateMeshIdInput('ab cd'), 'Use only letters, numbers, and hyphens.')
  assert.equal(
    validateMeshIdInput('abc-efgh'),
    'Use 4-character groups, like abcd-efgh-ijkl.',
  )
})
