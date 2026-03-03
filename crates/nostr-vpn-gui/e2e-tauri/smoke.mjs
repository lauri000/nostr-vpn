import { mkdirSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { setTimeout as delay } from 'node:timers/promises'

const DRIVER_PORT = Number(process.env.TAURI_DRIVER_PORT || '4444')
const DRIVER_BASE = `http://127.0.0.1:${DRIVER_PORT}`
const TAURI_DRIVER_BIN = process.env.TAURI_DRIVER_BIN || 'tauri-driver'
const APP_PATH = process.env.TAURI_APP || '/work/target/release/nostr-vpn-gui'
const NATIVE_DRIVER = process.env.NATIVE_DRIVER_PATH || '/usr/bin/WebKitWebDriver'
const SCREENSHOT_PATH =
  process.env.TAURI_E2E_SCREENSHOT || '/work/artifacts/screenshots/tauri-driver-smoke.png'

const VALID_NPUB =
  process.env.TAURI_E2E_PARTICIPANT ||
  'npub1dqgr6ds2kdauzpqtvpt2ldc5ca4spemj4n4jnjcvn7496x45gnesls5j6g'

const RELAY_URL = 'wss://relay.test.invalid'

let driver

function log(message) {
  console.log(`[tauri-e2e] ${message}`)
}

async function http(method, path, body) {
  const response = await fetch(`${DRIVER_BASE}${path}`, {
    method,
    headers: { 'content-type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  })

  const json = await response.json().catch(() => ({}))
  if (!response.ok || json.value?.error) {
    const detail = json.value?.message || JSON.stringify(json)
    throw new Error(`${method} ${path} failed: ${detail}`)
  }

  return json
}

async function waitForDriverReady(timeoutMs = 15_000) {
  const started = Date.now()

  while (Date.now() - started < timeoutMs) {
    try {
      const status = await fetch(`${DRIVER_BASE}/status`)
      if (status.ok) {
        return
      }
    } catch {
      // Keep polling.
    }

    await delay(250)
  }

  throw new Error('tauri-driver did not become ready')
}

function elementId(value) {
  return value['element-6066-11e4-a52e-4f735466cecf'] || value.ELEMENT
}

async function createSession() {
  const payload = {
    capabilities: {
      alwaysMatch: {
        browserName: 'wry',
        'tauri:options': {
          application: APP_PATH,
        },
      },
    },
  }

  const response = await http('POST', '/session', payload)
  const sessionId = response.value?.sessionId || response.sessionId
  if (!sessionId) {
    throw new Error(`Missing session id in response: ${JSON.stringify(response)}`)
  }

  return sessionId
}

async function find(sessionId, selector) {
  const response = await http('POST', `/session/${sessionId}/element`, {
    using: 'css selector',
    value: selector,
  })

  const id = elementId(response.value)
  if (!id) {
    throw new Error(`No element id for selector ${selector}`)
  }

  return id
}

async function findAll(sessionId, selector) {
  const response = await http('POST', `/session/${sessionId}/elements`, {
    using: 'css selector',
    value: selector,
  })

  return (response.value || []).map((entry) => elementId(entry)).filter(Boolean)
}

async function click(sessionId, id) {
  await http('POST', `/session/${sessionId}/element/${id}/click`, {})
}

async function clear(sessionId, id) {
  await http('POST', `/session/${sessionId}/element/${id}/clear`, {})
}

async function sendKeys(sessionId, id, text) {
  await http('POST', `/session/${sessionId}/element/${id}/value`, {
    text,
    value: [...text],
  })
}

async function getAttribute(sessionId, id, name) {
  const response = await http('GET', `/session/${sessionId}/element/${id}/attribute/${name}`)
  return response.value
}

async function screenshot(sessionId) {
  const response = await http('GET', `/session/${sessionId}/screenshot`)
  return response.value
}

async function source(sessionId) {
  const response = await http('GET', `/session/${sessionId}/source`)
  return response.value || ''
}

async function waitUntil(fn, description, timeoutMs = 10_000) {
  const started = Date.now()
  while (Date.now() - started < timeoutMs) {
    const value = await fn()
    if (value) {
      return value
    }

    await delay(200)
  }

  throw new Error(`Timed out waiting for ${description}`)
}

async function main() {
  log(`starting tauri-driver with ${TAURI_DRIVER_BIN} and native driver ${NATIVE_DRIVER}`)

  driver = spawn(TAURI_DRIVER_BIN, ['--port', `${DRIVER_PORT}`, '--native-driver', NATIVE_DRIVER], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env,
      TAURI_AUTOMATION: 'true',
    },
  })

  driver.stdout.on('data', (chunk) => {
    process.stdout.write(`[tauri-driver] ${chunk}`)
  })
  driver.stderr.on('data', (chunk) => {
    process.stderr.write(`[tauri-driver] ${chunk}`)
  })

  await Promise.race([
    waitForDriverReady(),
    new Promise((_, reject) => {
      driver.once('error', (error) => {
        reject(new Error(`failed to start tauri-driver: ${error.message}`))
      })
    }),
  ])

  const sessionId = await createSession()
  log(`webdriver session started: ${sessionId}`)

  try {
    await waitUntil(
      async () => {
        try {
          await find(sessionId, '[data-testid="pubkey"]')
          return true
        } catch {
          return false
        }
      },
      'pubkey to render',
    )

    const initialParticipantRows = (await findAll(sessionId, '[data-testid="participant-row"]')).length
    const participantInput = await find(sessionId, '[data-testid="participant-input"]')
    const participantAdd = await find(sessionId, '[data-testid="participant-add"]')
    await clear(sessionId, participantInput)
    await sendKeys(sessionId, participantInput, VALID_NPUB)
    await click(sessionId, participantAdd)

    await waitUntil(
      async () => {
        const rows = await findAll(sessionId, '[data-testid="participant-row"]')
        return rows.length >= initialParticipantRows + 1
      },
      'participant row to be added',
    )

    const initialRelayRows = (await findAll(sessionId, '[data-testid="relay-row"]')).length
    const relayInput = await find(sessionId, '[data-testid="relay-input"]')
    const relayAdd = await find(sessionId, '[data-testid="relay-add"]')
    await clear(sessionId, relayInput)
    await sendKeys(sessionId, relayInput, RELAY_URL)
    await click(sessionId, relayAdd)

    await waitUntil(
      async () => {
        const rows = await findAll(sessionId, '[data-testid="relay-row"]')
        return rows.length >= initialRelayRows + 1
      },
      'relay row to be added',
    )

    const removeButtons = await findAll(sessionId, '[data-testid="relay-remove"]')
    const lastRemoveButton = removeButtons.at(-1)
    if (!lastRemoveButton) {
      throw new Error('expected relay remove button to exist')
    }

    await click(sessionId, lastRemoveButton)
    await waitUntil(
      async () => {
        const rows = await findAll(sessionId, '[data-testid="relay-row"]')
        return rows.length === initialRelayRows
      },
      'relay row removal',
    )

    const nodeNameInput = await find(sessionId, '[data-testid="node-name-input"]')
    await clear(sessionId, nodeNameInput)
    await sendKeys(sessionId, nodeNameInput, 'tauri-driver-e2e-node')

    await waitUntil(
      async () => {
        const value = await getAttribute(sessionId, nodeNameInput, 'value')
        return value === 'tauri-driver-e2e-node'
      },
      'node name input update',
    )

    const screenshotBase64 = await screenshot(sessionId)
    mkdirSync(path.dirname(SCREENSHOT_PATH), { recursive: true })
    writeFileSync(SCREENSHOT_PATH, Buffer.from(screenshotBase64, 'base64'))
    log(`screenshot written: ${SCREENSHOT_PATH}`)

    log('tauri-driver smoke test passed')
  } catch (error) {
    const failureScreenshotPath = SCREENSHOT_PATH.replace(/\.png$/i, '-failure.png')
    try {
      const screenshotBase64 = await screenshot(sessionId)
      mkdirSync(path.dirname(failureScreenshotPath), { recursive: true })
      writeFileSync(failureScreenshotPath, Buffer.from(screenshotBase64, 'base64'))
      log(`failure screenshot written: ${failureScreenshotPath}`)
    } catch (screenshotError) {
      log(`failed to capture failure screenshot: ${String(screenshotError)}`)
    }

    try {
      const html = await source(sessionId)
      log(`page source snippet: ${html.slice(0, 600)}`)
    } catch (sourceError) {
      log(`failed to capture page source: ${String(sourceError)}`)
    }

    throw error
  } finally {
    await http('DELETE', `/session/${sessionId}`).catch(() => {})
  }
}

main()
  .catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
  .finally(async () => {
    if (driver && !driver.killed) {
      driver.kill('SIGTERM')
      await delay(500)
      if (!driver.killed) {
        driver.kill('SIGKILL')
      }
    }
  })
