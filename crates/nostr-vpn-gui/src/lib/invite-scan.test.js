import test from 'node:test'
import assert from 'node:assert/strict'

import {
  buildInviteScanConstraintCandidates,
  isRecoverableInviteScanError,
  openInviteScanStream,
} from './invite-scan.js'

test('buildInviteScanConstraintCandidates prefers environment-facing camera on mobile', () => {
  const candidates = buildInviteScanConstraintCandidates({ mobile: true })

  assert.equal(candidates[0].video.facingMode.ideal, 'environment')
  assert.equal(candidates.at(-1).video, true)
})

test('buildInviteScanConstraintCandidates keeps desktop fallback generic', () => {
  const candidates = buildInviteScanConstraintCandidates({ mobile: false })

  assert.equal(candidates.length, 2)
  assert.equal(candidates[0].video.width.ideal, 1280)
  assert.equal(candidates[1].video, true)
})

test('isRecoverableInviteScanError only retries recoverable constraint failures', () => {
  assert.equal(isRecoverableInviteScanError({ name: 'OverconstrainedError' }), true)
  assert.equal(isRecoverableInviteScanError({ name: 'NotAllowedError' }), false)
})

test('openInviteScanStream falls back to a generic camera request after constraint failure', async () => {
  const seen = []
  const stream = { id: 'stream-1' }
  const candidates = buildInviteScanConstraintCandidates({ mobile: true })

  const opened = await openInviteScanStream(async (constraints) => {
    seen.push(constraints)
    if (seen.length === 1) {
      const error = new Error('unsupported facing mode')
      error.name = 'OverconstrainedError'
      throw error
    }
    return stream
  }, candidates)

  assert.equal(opened, stream)
  assert.deepEqual(seen, [candidates[0], candidates[1]])
})

test('openInviteScanStream surfaces permission denial without retrying', async () => {
  const candidates = buildInviteScanConstraintCandidates({ mobile: false })
  let attempts = 0

  await assert.rejects(
    () =>
      openInviteScanStream(async () => {
        attempts += 1
        const error = new Error('permission denied')
        error.name = 'NotAllowedError'
        throw error
      }, candidates),
    /permission denied/,
  )

  assert.equal(attempts, 1)
})
