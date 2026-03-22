export const DARK_THEME_COLOR = '#0f0f0f'
export const LIGHT_THEME_COLOR = '#f4f5f7'

function defaultRoot() {
  return typeof document === 'undefined' ? null : document.documentElement
}

function defaultMetaTheme() {
  return typeof document === 'undefined'
    ? null
    : document.querySelector('meta[name="theme-color"]')
}

async function safeCurrentWindow(loadCurrentWindow) {
  if (typeof loadCurrentWindow !== 'function') {
    return null
  }

  try {
    return await loadCurrentWindow()
  } catch {
    return null
  }
}

function safeMediaQuery() {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return null
  }

  return window.matchMedia('(prefers-color-scheme: dark)')
}

function normalizeTheme(theme) {
  return theme === 'dark' ? 'dark' : 'light'
}

function resolveBrowserTheme(mediaQuery) {
  return mediaQuery?.matches ? 'dark' : 'light'
}

export function resolveThemeColor(theme) {
  return theme === 'dark' ? DARK_THEME_COLOR : LIGHT_THEME_COLOR
}

export function applyTheme(theme, options = {}) {
  const root = options.root ?? defaultRoot()
  const metaTheme =
    'metaTheme' in options ? options.metaTheme : defaultMetaTheme()
  const normalizedTheme = normalizeTheme(theme)

  if (root) {
    root.dataset.theme = normalizedTheme
    root.style.colorScheme = normalizedTheme
  }

  if (metaTheme) {
    metaTheme.setAttribute('content', resolveThemeColor(normalizedTheme))
  }

  return normalizedTheme
}

export async function startSystemThemeSync(options = {}) {
  const root = options.root ?? defaultRoot()
  const metaTheme =
    'metaTheme' in options ? options.metaTheme : defaultMetaTheme()
  const currentWindow =
    options.currentWindow ??
    (await safeCurrentWindow(options.loadCurrentWindow))
  const mediaQuery = options.mediaQuery ?? safeMediaQuery()
  const cleanups = []

  let activeTheme = resolveBrowserTheme(mediaQuery)

  if (currentWindow?.theme) {
    try {
      const tauriTheme = await currentWindow.theme()
      if (tauriTheme === 'dark' || tauriTheme === 'light') {
        activeTheme = tauriTheme
      }
    } catch {
      activeTheme = resolveBrowserTheme(mediaQuery)
    }
  }

  applyTheme(activeTheme, { root, metaTheme })

  if (currentWindow?.onThemeChanged) {
    try {
      const unlisten = await currentWindow.onThemeChanged(({ payload }) => {
        applyTheme(payload, { root, metaTheme })
      })
      cleanups.push(unlisten)
    } catch {
      // Fall back to browser theme events below when Tauri is unavailable.
    }
  }

  if (mediaQuery) {
    const handleChange = (event) => {
      applyTheme(event.matches ? 'dark' : 'light', { root, metaTheme })
    }

    if (typeof mediaQuery.addEventListener === 'function') {
      mediaQuery.addEventListener('change', handleChange)
      cleanups.push(() => mediaQuery.removeEventListener('change', handleChange))
    } else if (typeof mediaQuery.addListener === 'function') {
      mediaQuery.addListener(handleChange)
      cleanups.push(() => mediaQuery.removeListener(handleChange))
    }
  }

  return async () => {
    for (const cleanup of cleanups.reverse()) {
      await cleanup?.()
    }
  }
}
