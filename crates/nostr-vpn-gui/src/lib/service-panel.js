const serviceEnableRecommended = (state) =>
  !!state?.serviceEnablementSupported && !!state?.serviceInstalled && !!state?.serviceDisabled

export const shouldRenderServicePanel = (state, serviceActionInFlight = false) =>
  !!(serviceActionInFlight || state?.serviceSupported)

export const servicePanelKicker = ({
  serviceActionInFlight = false,
  serviceRepairPromptRecommended = false,
  serviceSetupRequired = false,
} = {}) =>
  serviceActionInFlight || serviceRepairPromptRecommended || serviceSetupRequired
    ? 'Action needed'
    : 'System'

export const servicePanelActions = (
  state,
  {
    serviceRepairPromptRecommended = false,
    serviceRepairRetryAfterInstall = false,
    serviceSetupRequired = false,
  } = {},
) => {
  const installed = !!state?.serviceInstalled
  const enableRecommended = serviceEnableRecommended(state)

  const actions = [
    serviceRepairPromptRecommended
      ? {
          key: 'repair',
          label:
            serviceRepairRetryAfterInstall && !state?.sessionActive
              ? 'Reinstall service and retry'
              : 'Reinstall service',
          accent: true,
        }
      : enableRecommended
        ? {
            key: 'enable',
            label: 'Enable service',
            accent: true,
          }
        : {
            key: installed ? 'reinstall' : 'install',
            label: installed ? 'Reinstall service' : 'Install service',
            accent: serviceSetupRequired,
          },
  ]

  if (enableRecommended && installed) {
    actions.push({
      key: 'reinstall',
      label: 'Reinstall service',
      accent: false,
    })
  } else if (!!state?.serviceEnablementSupported && installed && !state?.serviceDisabled) {
    actions.push({
      key: 'disable',
      label: 'Disable service',
      accent: false,
    })
  }

  if (installed) {
    actions.push({
      key: 'uninstall',
      label: 'Uninstall',
      accent: false,
    })
  }

  return actions
}
