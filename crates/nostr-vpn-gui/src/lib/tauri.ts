import { invoke } from '@tauri-apps/api/core'

import {
  addAdminMock,
  addNetworkMock,
  addParticipantMock,
  addRelayMock,
  acceptJoinRequestMock,
  connectSessionMock,
  disableSystemServiceMock,
  disconnectSessionMock,
  enableSystemServiceMock,
  importNetworkInviteMock,
  installCliMock,
  installSystemServiceMock,
  isAutostartEnabledMock,
  isTauriRuntime,
  removeAdminMock,
  removeNetworkMock,
  removeParticipantMock,
  removeRelayMock,
  renameNetworkMock,
  requestNetworkJoinMock,
  setAutostartEnabledMock,
  setNetworkEnabledMock,
  setNetworkJoinRequestsEnabledMock,
  setNetworkMeshIdMock,
  setParticipantAliasMock,
  startLanPairingMock,
  tickMock,
  uninstallCliMock,
  uninstallSystemServiceMock,
  stopLanPairingMock,
  updateSettingsMock,
} from './mock-backend.js'
import type { SettingsPatch, UiState } from './types'

const callUi = (method: string, mockFn: () => Promise<UiState> | UiState, args?: unknown) =>
  isTauriRuntime() ? invoke<UiState>(method, args as Record<string, unknown> | undefined) : mockFn()

export const tick = () => callUi('tick', tickMock)
export const connectSession = () => callUi('connect_session', connectSessionMock)
export const disconnectSession = () => callUi('disconnect_session', disconnectSessionMock)
export const installCli = () => callUi('install_cli', installCliMock)
export const uninstallCli = () => callUi('uninstall_cli', uninstallCliMock)
export const installSystemService = () =>
  callUi('install_system_service', installSystemServiceMock)
export const enableSystemService = () => callUi('enable_system_service', enableSystemServiceMock)
export const disableSystemService = () =>
  callUi('disable_system_service', disableSystemServiceMock)
export const uninstallSystemService = () =>
  callUi('uninstall_system_service', uninstallSystemServiceMock)
export const addNetwork = (name: string) => callUi('add_network', () => addNetworkMock(name), { name })
export const renameNetwork = (networkId: string, name: string) =>
  callUi('rename_network', () => renameNetworkMock(networkId, name), { networkId, name })
export const setNetworkMeshId = (networkId: string, meshId: string) =>
  callUi('set_network_mesh_id', () => setNetworkMeshIdMock(networkId, meshId), { networkId, meshId })
export const removeNetwork = (networkId: string) =>
  callUi('remove_network', () => removeNetworkMock(networkId), { networkId })
export const setNetworkEnabled = (networkId: string, enabled: boolean) =>
  callUi('set_network_enabled', () => setNetworkEnabledMock(networkId, enabled), {
    networkId,
    enabled,
  })
export const setNetworkJoinRequestsEnabled = (networkId: string, enabled: boolean) =>
  callUi(
    'set_network_join_requests_enabled',
    () => setNetworkJoinRequestsEnabledMock(networkId, enabled),
    { networkId, enabled },
  )
export const requestNetworkJoin = (networkId: string) =>
  callUi('request_network_join', () => requestNetworkJoinMock(networkId), { networkId })
export const addParticipant = (networkId: string, npub: string, alias = '') =>
  callUi('add_participant', () => addParticipantMock(networkId, npub, alias), {
    networkId,
    npub,
    alias: alias.trim() || null,
  })
export const addAdmin = (networkId: string, npub: string) =>
  callUi('add_admin', () => addAdminMock(networkId, npub), { networkId, npub })
export const importNetworkInvite = (invite: string) =>
  callUi('import_network_invite', () => importNetworkInviteMock(invite), { invite })
export const startLanPairing = () => callUi('start_lan_pairing', startLanPairingMock)
export const stopLanPairing = () => callUi('stop_lan_pairing', stopLanPairingMock)
export const removeParticipant = (networkId: string, npub: string) =>
  callUi('remove_participant', () => removeParticipantMock(networkId, npub), { networkId, npub })
export const removeAdmin = (networkId: string, npub: string) =>
  callUi('remove_admin', () => removeAdminMock(networkId, npub), { networkId, npub })
export const acceptJoinRequest = (networkId: string, requesterNpub: string) =>
  callUi('accept_join_request', () => acceptJoinRequestMock(networkId, requesterNpub), {
    networkId,
    requesterNpub,
  })
export const setParticipantAlias = (npub: string, alias: string) =>
  callUi('set_participant_alias', () => setParticipantAliasMock(npub, alias), { npub, alias })
export const addRelay = (relay: string) => callUi('add_relay', () => addRelayMock(relay), { relay })
export const removeRelay = (relay: string) =>
  callUi('remove_relay', () => removeRelayMock(relay), { relay })
export const updateSettings = (patch: SettingsPatch) =>
  callUi('update_settings', () => updateSettingsMock(patch), { patch })

export const isAutostartEnabled = async () => {
  if (!isTauriRuntime()) {
    return isAutostartEnabledMock()
  }

  try {
    const { isEnabled } = await import('@tauri-apps/plugin-autostart')
    return await isEnabled()
  } catch {
    return false
  }
}

export const setAutostartEnabled = async (enabled: boolean) => {
  if (!isTauriRuntime()) {
    return setAutostartEnabledMock(enabled)
  }

  try {
    if (enabled) {
      const { enable } = await import('@tauri-apps/plugin-autostart')
      await enable()
    } else {
      const { disable } = await import('@tauri-apps/plugin-autostart')
      await disable()
    }
    return true
  } catch {
    return false
  }
}
