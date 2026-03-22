import test from 'node:test'
import assert from 'node:assert/strict'

import {
  applyTheme,
  resolveThemeColor,
  startSystemThemeSync,
} from './theme.js'

test('applyTheme updates dataset, color-scheme, and meta theme-color', () => {
  const root = {
    dataset: {},
    style: {
      colorScheme: '',
    },
  }
  const metaTheme = {
    content: '',
    setAttribute(name, value) {
      if (name === 'content') {
        this.content = value
      }
    },
  }

  applyTheme('light', { root, metaTheme })

  assert.equal(root.dataset.theme, 'light')
  assert.equal(root.style.colorScheme, 'light')
  assert.equal(metaTheme.content, resolveThemeColor('light'))
})

test('startSystemThemeSync applies the current theme and reacts to theme changes', async () => {
  const root = {
    dataset: {},
    style: {
      colorScheme: '',
    },
  }
  const metaTheme = {
    content: '',
    setAttribute(name, value) {
      if (name === 'content') {
        this.content = value
      }
    },
  }

  const listeners = new Set()
  const currentWindow = {
    async theme() {
      return 'dark'
    },
    async onThemeChanged(handler) {
      listeners.add(handler)
      return () => listeners.delete(handler)
    },
  }

  const stop = await startSystemThemeSync({
    currentWindow,
    root,
    metaTheme,
  })

  assert.equal(root.dataset.theme, 'dark')
  assert.equal(metaTheme.content, resolveThemeColor('dark'))

  for (const listener of listeners) {
    listener({ payload: 'light' })
  }

  assert.equal(root.dataset.theme, 'light')
  assert.equal(root.style.colorScheme, 'light')
  assert.equal(metaTheme.content, resolveThemeColor('light'))

  await stop()
  assert.equal(listeners.size, 0)
})

test('startSystemThemeSync falls back to light when Tauri returns null', async () => {
  const root = {
    dataset: {},
    style: {
      colorScheme: '',
    },
  }

  const currentWindow = {
    async theme() {
      return null
    },
    async onThemeChanged() {
      return () => {}
    },
  }

  const stop = await startSystemThemeSync({
    currentWindow,
    root,
    metaTheme: null,
  })

  assert.equal(root.dataset.theme, 'light')

  await stop()
})

test('startSystemThemeSync falls back to browser theme when loading the Tauri window API fails', async () => {
  const root = {
    dataset: {},
    style: {
      colorScheme: '',
    },
  }

  let loadAttempts = 0
  const mediaQuery = {
    matches: true,
    addEventListener() {},
    removeEventListener() {},
  }

  const stop = await startSystemThemeSync({
    loadCurrentWindow: async () => {
      loadAttempts += 1
      throw new Error('window api is not available on mobile')
    },
    mediaQuery,
    root,
    metaTheme: null,
  })

  assert.equal(loadAttempts, 1)
  assert.equal(root.dataset.theme, 'dark')
  assert.equal(root.style.colorScheme, 'dark')

  await stop()
})
