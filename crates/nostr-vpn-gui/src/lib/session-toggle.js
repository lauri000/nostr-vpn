export const sessionToggleVisualState = (sessionActive, pendingTarget = null) => {
  const active = typeof pendingTarget === 'boolean' ? pendingTarget : Boolean(sessionActive)
  return {
    active,
    className: active ? 'on' : 'off',
    label: `VPN ${active ? 'On' : 'Off'}`,
  }
}
