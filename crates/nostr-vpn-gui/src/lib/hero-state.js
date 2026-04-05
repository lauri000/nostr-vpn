/**
 * @typedef {import('./types').UiState} UiState
 */

/**
 * @param {UiState} state
 * @param {{ serviceInstallRecommended?: boolean, serviceEnableRecommended?: boolean }} options
 * @returns {'Service required' | 'Connected' | `Mesh ${number}/${number}` | 'VPN On' | 'VPN Off'}
 */
export function heroStateText(state, options = {}) {
  const serviceInstallRecommended = options.serviceInstallRecommended ?? false
  const serviceEnableRecommended = options.serviceEnableRecommended ?? false

  if ((serviceInstallRecommended || serviceEnableRecommended) && !state.sessionActive) {
    return 'Service required'
  }
  if (state.meshReady) {
    return 'Connected'
  }
  if (state.sessionActive) {
    const connectedPeerCount = Number(state.connectedPeerCount ?? 0)
    const expectedPeerCount = Number(state.expectedPeerCount ?? 0)
    if (expectedPeerCount > 0) {
      return `Mesh ${connectedPeerCount}/${expectedPeerCount}`
    }
    return 'VPN On'
  }
  return 'VPN Off'
}

/**
 * @param {UiState} state
 * @returns {string}
 */
export function heroStatusDetailText(state) {
  const sessionStatus = state.sessionStatus?.trim() ?? ''

  if (!sessionStatus) {
    return ''
  }

  return sessionStatus
}
