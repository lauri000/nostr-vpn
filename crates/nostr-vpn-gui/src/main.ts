import './app.css'
import { mount } from 'svelte'
import {
  attachBootReadyHandler,
  describeBootError,
  fatalBootMarkup,
} from './lib/boot.js'

type BootWindow = Window & {
  __NVPN_HIDE_BOOT_SPLASH__?: () => void
}

let stopSystemThemeSync = async () => {}
let app: ReturnType<typeof mount> | null = null
const target = document.getElementById('app')
const detachBootReadyHandler = attachBootReadyHandler(window, () => {
  ;(window as BootWindow).__NVPN_HIDE_BOOT_SPLASH__?.()
})

if (!target) {
  throw new Error('Missing #app root element')
}

let bootFailed = false

const renderBootFailure = (summary: string, error: unknown) => {
  if (bootFailed) {
    return
  }

  bootFailed = true
  target.innerHTML = fatalBootMarkup(summary, describeBootError(error))
}

window.addEventListener('error', (event) => {
  renderBootFailure('Frontend bootstrap failed', event.error ?? event.message)
})

window.addEventListener('unhandledrejection', (event) => {
  renderBootFailure('Unhandled promise rejection', event.reason)
})

const bootstrap = async () => {
  const [{ default: App }, { startSystemThemeSync }] = await Promise.all([
    import('./App.svelte'),
    import('./lib/theme.js'),
  ])

  stopSystemThemeSync = await startSystemThemeSync()
  target.innerHTML = ''
  app = mount(App, { target })
}

void bootstrap().catch((error) => {
  renderBootFailure('Frontend bootstrap failed', error)
})

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    detachBootReadyHandler()
    void stopSystemThemeSync()
  })
}

export default app
