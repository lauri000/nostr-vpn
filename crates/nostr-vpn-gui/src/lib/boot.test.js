import test from 'node:test'
import assert from 'node:assert/strict'

import {
  attachBootReadyHandler,
  describeBootError,
  dispatchBootReady,
  fatalBootMarkup,
  waitForNextPaint,
} from './boot.js'

test('describeBootError prefers error stack information', () => {
  const error = new Error('boom')
  error.stack = 'Error: boom\n    at bootstrap'

  assert.equal(describeBootError(error), 'Error: boom\n    at bootstrap')
})

test('describeBootError handles strings and circular objects', () => {
  assert.equal(describeBootError('plain failure'), 'plain failure')

  const circular = {}
  circular.self = circular

  assert.match(describeBootError(circular), /\[object Object\]/)
})

test('fatalBootMarkup escapes HTML-sensitive content', () => {
  const markup = fatalBootMarkup('Broken <UI>', 'boom & details')

  assert.match(markup, /Broken &lt;UI&gt;/)
  assert.match(markup, /boom &amp; details/)
})

test('boot splash hides only after the ready event fires', () => {
  const target = new EventTarget()
  let hideCount = 0

  const detach = attachBootReadyHandler(target, () => {
    hideCount += 1
  })

  target.dispatchEvent(new Event('unrelated'))
  assert.equal(hideCount, 0)

  dispatchBootReady(target)
  assert.equal(hideCount, 1)

  dispatchBootReady(target)
  assert.equal(hideCount, 1)

  detach()
})

test('waitForNextPaint waits for two animation frames when available', async () => {
  const frames = []
  const target = {
    requestAnimationFrame(callback) {
      frames.push(callback)
      return frames.length
    },
  }

  let resolved = false
  const paint = waitForNextPaint(target).then(() => {
    resolved = true
  })

  assert.equal(frames.length, 1)
  assert.equal(resolved, false)

  frames.shift()()
  await Promise.resolve()
  assert.equal(frames.length, 1)
  assert.equal(resolved, false)

  frames.shift()()
  await paint
  assert.equal(resolved, true)
})
