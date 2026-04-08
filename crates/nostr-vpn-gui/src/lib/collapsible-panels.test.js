import test from 'node:test'
import assert from 'node:assert/strict'

import { reconcileAutoOpenPanelState } from './collapsible-panels.js'

test('reconcileAutoOpenPanelState starts open when warnings already exist', () => {
  assert.equal(reconcileAutoOpenPanelState(false, null, 2), true)
})

test('reconcileAutoOpenPanelState preserves a manually opened panel across clean refreshes', () => {
  assert.equal(reconcileAutoOpenPanelState(true, 0, 0), true)
})

test('reconcileAutoOpenPanelState auto-opens when warnings appear later', () => {
  assert.equal(reconcileAutoOpenPanelState(false, 0, 1), true)
})

test('reconcileAutoOpenPanelState preserves a manual close until a new warning appears', () => {
  assert.equal(reconcileAutoOpenPanelState(false, 2, 2), false)
})
