const STALE_SERVICE_MARKERS = [
  'restart the daemon with a newer nvpn binary',
  'older nvpn daemon binary is still running',
]

const normalizedError = (error) => String(error ?? '').trim()
const staleServiceError = (error) =>
  STALE_SERVICE_MARKERS.some((marker) => normalizedError(error).toLowerCase().includes(marker))

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

  return serviceBinaryVersionMismatch(state) || staleServiceError(error)
}

export const serviceRepairRetryRecommended = (error) => staleServiceError(error)

export const serviceRepairErrorText = (error, state) => {
  const normalized = normalizedError(error)
  const staleError = staleServiceError(error)
  const versionMismatch = serviceBinaryVersionMismatch(state)

  if (!(staleError || (!normalized && versionMismatch))) {
    return normalizedError(error)
  }

  return 'Background service is out of date. Reinstall it, then try turning VPN on again.'
}
