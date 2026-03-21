const escapeHtml = (value) =>
  value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')

export const BOOT_READY_EVENT = 'nvpn:boot-ready'

export function describeBootError(error) {
  if (error instanceof Error) {
    return error.stack || `${error.name}: ${error.message}`
  }

  if (typeof error === 'string') {
    return error
  }

  try {
    return JSON.stringify(error, null, 2)
  } catch {
    return String(error)
  }
}

export function fatalBootMarkup(title, detail) {
  return `
    <section style="min-height:100vh;padding:32px 20px;background:#0f0f0f;color:#f1f1f1;font-family:'IBM Plex Sans','Segoe UI',sans-serif;">
      <div style="max-width:760px;margin:0 auto;">
        <div style="font-size:12px;letter-spacing:0.12em;text-transform:uppercase;color:#75ecff;margin-bottom:12px;">Nostr VPN</div>
        <h1 style="margin:0 0 12px;font-size:28px;line-height:1.1;">${escapeHtml(title)}</h1>
        <p style="margin:0 0 18px;color:#d4d4d4;line-height:1.5;">The frontend failed before it could render the main UI.</p>
        <pre style="margin:0;padding:16px;border-radius:12px;background:#181818;border:1px solid rgba(255,255,255,0.12);white-space:pre-wrap;word-break:break-word;font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;">${escapeHtml(detail)}</pre>
      </div>
    </section>
  `
}

export function attachBootReadyHandler(target, hide) {
  const onReady = () => {
    hide()
  }

  target.addEventListener(BOOT_READY_EVENT, onReady, { once: true })

  return () => {
    target.removeEventListener(BOOT_READY_EVENT, onReady)
  }
}

export function dispatchBootReady(target) {
  target.dispatchEvent(new Event(BOOT_READY_EVENT))
}

export function waitForNextPaint(target) {
  if (typeof target?.requestAnimationFrame === 'function') {
    return new Promise((resolve) => {
      target.requestAnimationFrame(() => {
        target.requestAnimationFrame(() => {
          resolve()
        })
      })
    })
  }

  return new Promise((resolve) => {
    setTimeout(resolve, 0)
  })
}
