import test from 'node:test'
import assert from 'node:assert/strict'

import { sessionToggleVisualState } from './session-toggle.js'

test('sessionToggleVisualState uses the live session state when nothing is pending', () => {
  assert.deepEqual(sessionToggleVisualState(false), {
    active: false,
    className: 'off',
    label: 'VPN Off',
  })
  assert.deepEqual(sessionToggleVisualState(true), {
    active: true,
    className: 'on',
    label: 'VPN On',
  })
})

test('sessionToggleVisualState prefers the pending target while a toggle is in flight', () => {
  assert.deepEqual(sessionToggleVisualState(false, true), {
    active: true,
    className: 'on',
    label: 'VPN On',
  })
  assert.deepEqual(sessionToggleVisualState(true, false), {
    active: false,
    className: 'off',
    label: 'VPN Off',
  })
})
