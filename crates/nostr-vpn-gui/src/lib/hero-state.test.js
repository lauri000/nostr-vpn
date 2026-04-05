import test from 'node:test'
import assert from 'node:assert/strict'

import { heroStateText, heroStatusDetailText } from './hero-state.js'

function baseState() {
  return {
    sessionActive: false,
    meshReady: false,
    relayConnected: false,
    sessionStatus: '',
    connectedPeerCount: 0,
    expectedPeerCount: 0,
  }
}

test('heroStateText reports service required before disconnected when service setup blocks startup', () => {
  const state = baseState()

  assert.equal(
    heroStateText(state, { serviceInstallRecommended: true }),
    'Service required'
  )
  assert.equal(
    heroStateText(state, { serviceEnableRecommended: true }),
    'Service required'
  )
})

test('heroStateText reports connected only when the mesh is ready', () => {
  const state = {
    ...baseState(),
    sessionActive: true,
    meshReady: true,
  }

  assert.equal(heroStateText(state), 'Connected')
})

test('heroStateText reports mesh counts for active sessions without a ready mesh', () => {
  const state = {
    ...baseState(),
    sessionActive: true,
    connectedPeerCount: 0,
    expectedPeerCount: 3,
  }

  assert.equal(heroStateText(state), 'Mesh 0/3')
})

test('heroStateText reports VPN off for inactive sessions without service blockers', () => {
  assert.equal(heroStateText(baseState()), 'VPN Off')
})

test('heroStateText reports VPN on when active without configured remote peers', () => {
  const state = {
    ...baseState(),
    sessionActive: true,
  }

  assert.equal(heroStateText(state), 'VPN On')
})

test('heroStatusDetailText keeps status text visible', () => {
  const state = {
    ...baseState(),
    sessionActive: true,
    meshReady: true,
    relayConnected: false,
    sessionStatus: 'Connected',
  }

  assert.equal(heroStatusDetailText(state), 'Connected')
})

test('heroStatusDetailText keeps non-paused status details visible', () => {
  const state = {
    ...baseState(),
    sessionActive: true,
    meshReady: false,
    relayConnected: false,
    sessionStatus: 'Relay connect failed; retry in 5s',
  }

  assert.equal(heroStatusDetailText(state), 'Relay connect failed; retry in 5s')
})
