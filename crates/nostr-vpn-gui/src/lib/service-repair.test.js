import test from 'node:test'
import assert from 'node:assert/strict'

import {
  serviceRepairErrorText,
  serviceRepairRecommended,
  serviceRepairRetryRecommended,
} from './service-repair.js'

const installedServiceState = {
  serviceSupported: true,
  serviceInstalled: true,
  serviceRunning: true,
  daemonRunning: true,
  appVersion: '0.2.28',
  daemonBinaryVersion: '0.2.27',
}

test('serviceRepairRecommended detects stale daemon mismatch errors for installed services', () => {
  assert.equal(
    serviceRepairRecommended(
      'nvpn resume failed stderr: Error: daemon did not acknowledge control request within 3s; restart the daemon with a newer nvpn binary',
      installedServiceState
    ),
    true
  )

  assert.equal(
    serviceRepairRecommended(
      'failed to resume VPN session: daemon acknowledged control request but did not reload; likely an older nvpn daemon binary is still running. restart or reinstall the app/service so the daemon matches the current CLI',
      installedServiceState
    ),
    true
  )
})

test('serviceRepairRetryRecommended only triggers on explicit stale daemon errors', () => {
  assert.equal(
    serviceRepairRetryRecommended(
      'daemon did not acknowledge control request within 3s; restart the daemon with a newer nvpn binary'
    ),
    true
  )
  assert.equal(serviceRepairRetryRecommended(''), false)
})

test('serviceRepairRecommended ignores stale daemon errors when no service is installed', () => {
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

test('serviceRepairErrorText rewrites raw mismatch errors into a repair instruction', () => {
  assert.equal(
    serviceRepairErrorText(
      'daemon acknowledged control request but did not reload; likely an older nvpn daemon binary is still running. restart or reinstall the app/service so the daemon matches the current CLI',
      installedServiceState
    ),
    'Background service is out of date. Reinstall it, then try turning VPN on again.'
  )
})

test('serviceRepairErrorText surfaces startup version mismatch without a raw error', () => {
  assert.equal(
    serviceRepairErrorText('', installedServiceState),
    'Background service is out of date. Reinstall it, then try turning VPN on again.'
  )
})

test('serviceRepairErrorText leaves unrelated errors unchanged', () => {
  assert.equal(
    serviceRepairErrorText('Clipboard copy failed', installedServiceState),
    'Clipboard copy failed'
  )
})
