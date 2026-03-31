const STALE_SERVICE_MARKERS = [
  'restart the daemon with a newer nvpn binary',
  'older nvpn daemon binary is still running',
]

const UNRESPONSIVE_SERVICE_MARKERS = [
  'daemon did not acknowledge control request',
  'daemon did not report result for',
  'daemon acknowledged control request but did not',
]

const normalizedError = (error) => String(error ?? '').trim()
const staleServiceError = (error) =>
  STALE_SERVICE_MARKERS.some((marker) => normalizedError(error).toLowerCase().includes(marker))
const unresponsiveServiceError = (error) =>
  UNRESPONSIVE_SERVICE_MARKERS.some((marker) =>
    normalizedError(error).toLowerCase().includes(marker)
  )

const serviceBinaryVersionMismatch = (state) => {
  if (
    !state?.serviceSupported ||
    !state?.serviceInstalled ||
    !state?.serviceRunning ||
    !state?.daemonRunning
  ) {
    return false
  }

  const appVersion = String(state.appVersion ?? '').trim()
  const daemonBinaryVersion = String(state.daemonBinaryVersion ?? '').trim()
  return appVersion.length > 0 && daemonBinaryVersion !== appVersion
}

export const serviceRepairRecommended = (error, state) => {
  if (!state?.serviceSupported || !state?.serviceInstalled) {
    return false
  }

  return serviceBinaryVersionMismatch(state)
}

export const serviceRepairRetryRecommended = (error) => unresponsiveServiceError(error)

export const serviceRepairErrorText = (error, state) => {
  const normalized = normalizedError(error)
  const versionMismatch = serviceBinaryVersionMismatch(state)
  const controlTimeout = unresponsiveServiceError(error)

  if (!normalized && versionMismatch) {
    return 'Background service is out of date. Reinstall it, then try turning VPN on again.'
  }

  if (normalized && versionMismatch && staleServiceError(error)) {
    return 'Background service is out of date. Reinstall it, then try turning VPN on again.'
  }

  if (controlTimeout) {
    return 'Background service did not respond in time. Try turning VPN on again. If it keeps happening, restart or reinstall the service.'
  }

  if (!normalized) {
    return normalizedError(error)
  }
  return normalized
}
