import test from 'node:test'
import assert from 'node:assert/strict'

import {
  NETWORK_INVITE_PREFIX,
  decodeInvitePayload,
  determineInviteImportTarget,
  encodeInvitePayload,
} from './invite-code.js'

test('encodeInvitePayload emits the app invite prefix and round-trips decoded values', () => {
  const payload = {
    v: 2,
    networkName: 'Home',
    networkId: 'mesh-home',
    inviterNpub: 'npub1x8teht3pj2zhq6e4l6s5zh2fcn0vzrp3d8zjls74g7zq5qemk3dq3wlp5m',
    admins: ['npub1x8teht3pj2zhq6e4l6s5zh2fcn0vzrp3d8zjls74g7zq5qemk3dq3wlp5m'],
    participants: ['npub1x8teht3pj2zhq6e4l6s5zh2fcn0vzrp3d8zjls74g7zq5qemk3dq3wlp5m'],
    relays: ['wss://relay.one.example', 'wss://relay.two.example'],
  }

  const invite = encodeInvitePayload(payload)

  assert.match(invite, new RegExp(`^${NETWORK_INVITE_PREFIX}`))
  assert.deepEqual(decodeInvitePayload(invite), payload)
})

test('decodeInvitePayload accepts JSON invite payloads from the real backend format', () => {
  const invite = JSON.stringify({
    v: 1,
    networkName: 'Home',
    networkId: 'mesh-home',
    inviterNpub: 'npub1x8teht3pj2zhq6e4l6s5zh2fcn0vzrp3d8zjls74g7zq5qemk3dq3wlp5m',
    relays: ['wss://relay.one.example'],
  })

  assert.deepEqual(decodeInvitePayload(invite), {
    v: 2,
    networkName: 'Home',
    networkId: 'mesh-home',
    inviterNpub: 'npub1x8teht3pj2zhq6e4l6s5zh2fcn0vzrp3d8zjls74g7zq5qemk3dq3wlp5m',
    admins: ['npub1x8teht3pj2zhq6e4l6s5zh2fcn0vzrp3d8zjls74g7zq5qemk3dq3wlp5m'],
    participants: ['npub1x8teht3pj2zhq6e4l6s5zh2fcn0vzrp3d8zjls74g7zq5qemk3dq3wlp5m'],
    relays: ['wss://relay.one.example'],
  })
})

test('determineInviteImportTarget prefers an existing network match before creating a new one', () => {
  const target = determineInviteImportTarget(
    [
      { id: 'network-1', name: 'Work', enabled: true, networkId: 'mesh-work', participants: [{}] },
      { id: 'network-2', name: 'Home', enabled: false, networkId: 'mesh-home', participants: [] },
    ],
    'network-1',
    'mesh-home',
  )

  assert.deepEqual(target, { mode: 'existing', networkId: 'network-2' })
})
