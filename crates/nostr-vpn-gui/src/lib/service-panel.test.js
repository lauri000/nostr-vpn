import test from 'node:test'
import assert from 'node:assert/strict'

import {
  servicePanelActions,
  servicePanelKicker,
  shouldRenderServicePanel,
} from './service-panel.js'

function baseState() {
  return {
    serviceSupported: true,
    serviceEnablementSupported: true,
    serviceInstalled: true,
    serviceDisabled: false,
    serviceRunning: true,
    sessionActive: false,
  }
}

test('shouldRenderServicePanel keeps service controls visible after setup completes', () => {
  assert.equal(shouldRenderServicePanel(baseState(), false), true)
})

test('servicePanelKicker only shows action-needed state when setup or repair is required', () => {
  assert.equal(servicePanelKicker({}), 'System')
  assert.equal(servicePanelKicker({ serviceSetupRequired: true }), 'Action needed')
})

test('servicePanelActions keeps reinstall available when the service is disabled', () => {
  const actions = servicePanelActions(
    {
      ...baseState(),
      serviceDisabled: true,
      serviceRunning: false,
    },
    {},
  )

  assert.deepEqual(
    actions.map((action) => action.key),
    ['enable', 'reinstall', 'uninstall'],
  )
})

test('servicePanelActions shows reinstall, disable, and uninstall for healthy installs', () => {
  const actions = servicePanelActions(baseState(), {})

  assert.deepEqual(
    actions.map((action) => action.key),
    ['reinstall', 'disable', 'uninstall'],
  )
})
