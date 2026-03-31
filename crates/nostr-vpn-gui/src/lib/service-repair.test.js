import test from 'node:test'
import assert from 'node:assert/strict'

import {
  serviceRepairErrorText,
  serviceRepairRecommended,
  serviceRepairRetryRecommended,
} from './service-repair.js'

const currentServiceState = {
  serviceSupported: true,
  serviceInstalled: true,
  serviceRunning: true,
  daemonRunning: true,
  appVersion: '0.2.28',
  daemonBinaryVersion: '0.2.28',
}

const installedServiceState = {
  ...currentServiceState,
  daemonBinaryVersion: '0.2.27',
}

test('serviceRepairRecommended detects stale daemon mismatch errors for installed services', () => {
  assert.equal(
    serviceRepairRecommended(
      'nvpn resume failed stderr: Error: daemon did not acknowledge control request within 3s; restart the daemon with a newer nvpn binary',
      currentServiceState
    ),
    false
  )

  assert.equal(
    serviceRepairRecommended(
      'failed to resume VPN session: daemon acknowledged control request but did not reload; likely an older nvpn daemon binary is still running. restart or reinstall the app/service so the daemon matches the current CLI',
      currentServiceState
    ),
    false
  )
})

test('serviceRepairRetryRecommended only triggers on explicit daemon control errors', () => {
  assert.equal(
    serviceRepairRetryRecommended(
      'daemon did not acknowledge control request within 3s; restart the daemon with a newer nvpn binary'
    ),
    true
  )
  assert.equal(serviceRepairRetryRecommended(''), false)
})

test('serviceRepairRecommended ignores daemon control errors when no service is installed', () => {
  assert.equal(
    serviceRepairRecommended(
      'daemon did not acknowledge control request within 3s; restart the daemon with a newer nvpn binary',
      {
        serviceSupported: true,
        serviceInstalled: false,
        serviceRunning: false,
        daemonRunning: false,
        appVersion: '0.2.28',
        daemonBinaryVersion: '',
      }
    ),
    false
  )
})

test('serviceRepairRecommended detects daemon and app version mismatch at startup', () => {
  assert.equal(serviceRepairRecommended('', installedServiceState), true)
})

test('serviceRepairErrorText surfaces a generic timeout message for daemon control errors', () => {
  assert.equal(
    serviceRepairErrorText(
      'daemon acknowledged control request but did not reload; likely an older nvpn daemon binary is still running. restart or reinstall the app/service so the daemon matches the current CLI',
      currentServiceState
    ),
    'Background service did not respond in time. Try turning VPN on again. If it keeps happening, restart or reinstall the service.'
  )
})

test('serviceRepairErrorText surfaces startup version mismatch without a raw error', () => {
  assert.equal(
    serviceRepairErrorText('', installedServiceState),
    'Background service is out of date. Reinstall it, then try turning VPN on again.'
  )
})

test('serviceRepairErrorText prefers a repair instruction when a control error also has a version mismatch', () => {
  assert.equal(
    serviceRepairErrorText(
      'daemon acknowledged control request but did not reload; likely an older nvpn daemon binary is still running. restart or reinstall the app/service so the daemon matches the current CLI',
      installedServiceState
    ),
    'Background service is out of date. Reinstall it, then try turning VPN on again.'
  )
})

test('serviceRepairErrorText leaves unrelated errors unchanged', () => {
  assert.equal(
    serviceRepairErrorText('Clipboard copy failed', installedServiceState),
    'Clipboard copy failed'
  )
})
