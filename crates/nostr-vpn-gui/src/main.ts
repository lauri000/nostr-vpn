import './app.css'
import { mount } from 'svelte'
import App from './App.svelte'
import { startSystemThemeSync } from './lib/theme.js'

let stopSystemThemeSync = async () => {}

void startSystemThemeSync().then((stop) => {
  stopSystemThemeSync = stop
})

const app = mount(App, {
  target: document.getElementById('app')!,
})

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    void stopSystemThemeSync()
  })
}

export default app
