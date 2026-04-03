<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { invoke } from '@tauri-apps/api/core'
  import { listen } from '@tauri-apps/api/event'
  import { Check, Copy, Trash2 } from 'lucide-svelte'

  import { dispatchBootReady, waitForNextPaint } from './lib/boot.js'
  import {
    lanPairingDeadlineFromSnapshot,
    remainingSecsFromDeadline,
  } from './lib/countdown.js'
  import { heroStateText, heroStatusDetailText } from './lib/hero-state.js'
  import {
    serviceRepairErrorText,
    serviceRepairRecommended,
    serviceRepairRetryRecommended,
  } from './lib/service-repair.js'
  import { parseAppDeepLink } from './lib/deep-link-actions.js'
  import {
    canonicalizeMeshIdInput,
    formatMeshIdDraftForDisplay,
    formatMeshIdForDisplay,
    validateMeshIdInput,
  } from './lib/mesh-id.js'
  import { nodeNameDnsPreview } from './lib/node-name.js'
  import {
    activeNetwork,
    formatCountdown,
    heroBadgeText,
    heroDetailText,
    heroStateBadgeClass,
    heroSubtext,
    inactiveNetworks,
    networkAdminSummary,
    networkPeerSummary,
    onlineDeviceSummary,
    participantBadgeClass,
    participantPresenceBadgeText,
    participantTransportBadgeText,
    platformLabel,
    short,
  } from './lib/app-view'
  import AdvancedPanels from './AdvancedPanels.svelte'
  import InviteShareSection from './InviteShareSection.svelte'
  import PublicServicesPanel from './PublicServicesPanel.svelte'
  import RoutingPanel from './RoutingPanel.svelte'
  import SavedNetworksPanel from './SavedNetworksPanel.svelte'
  import ServiceActionPanel from './ServiceActionPanel.svelte'
  import SystemPanel from './SystemPanel.svelte'
import {
  addAdmin,
  addNetwork,
  addParticipant,
  addRelay,
  acceptJoinRequest,
  connectSession,
  disableSystemService,
  disconnectSession,
  enableSystemService,
  importNetworkInvite,
  installCli,
  installSystemService,
  isAutostartEnabled,
  removeNetwork,
  removeAdmin,
  removeParticipant,
  removeRelay,
  renameNetwork,
  requestNetworkJoin,
  setNetworkEnabled,
  setNetworkJoinRequestsEnabled,
  setNetworkMeshId,
  setParticipantAlias,
  setAutostartEnabled,
  startLanPairing,
  stopLanPairing,
  tick,
  uninstallCli,
  uninstallSystemService,
  updateSettings,
} from './lib/tauri'
  import type {
    HealthIssue,
    NetworkView,
    ParticipantView,
    PeerState,
    PresenceState,
    SettingsPatch,
    UiState,
  } from './lib/types'

  let state: UiState | null = null
  let relayInput = ''
  let error = ''
  let cliActionStatus = ''
  let serviceActionStatus = ''
  let copiedValue: 'pubkey' | 'meshId' | 'invite' | 'peerNpub' | null = null
  let copiedPeerNpub: string | null = null

  let newNetworkName = ''
  let nodeNameDraft = ''
  let endpointDraft = ''
  let tunnelIpDraft = ''
  let listenPortDraft = ''
  let exitNodeDraft = ''
  let advertisedRoutesDraft = ''
  let magicDnsSuffixDraft = ''
  let exitNodeSearch = ''
  let draftsInitialized = false
  let showAdvancedRoutes = false

  let networkNameDrafts: Record<string, string> = {}
  let networkIdDrafts: Record<string, string> = {}
  let networkIdErrors: Record<string, string> = {}
  let participantInputDrafts: Record<string, string> = {}
  let participantAddAliasDrafts: Record<string, string> = {}
  let participantAliasDrafts: Record<string, string> = {}

  let autostartReady = false
  let autostartUpdating = false

  const debouncers = new Map<string, number>()
  let pollHandle: number | null = null
  let lanPairingTickHandle: number | null = null
  let copiedHandle: number | null = null
  let deepLinkUnlisten: (() => void) | null = null
  let refreshInFlight = false
  let actionInFlight = false
  let serviceInstallRecommended = false
  let serviceEnableRecommended = false
  let serviceRepairPromptRecommended = false
  let serviceRepairRetryAfterInstall = false
  let serviceRepairPromptShownFor = ''
  let serviceRepairPromptInFlight = false
  let serviceSetupRequired = false
  let vpnControlSupported = false
  let cliInstallSupported = false
  let startupSettingsSupported = false
  let trayBehaviorSupported = false
  let bootReadyDispatched = false
  let appDisposed = false
  let lanPairingDeadlineMs: number | null = null
  let lanPairingDisplayRemainingSecs = 0
  const processedDeepLinks = new Set<string>()

  const NETWORK_MESH_ID_IDLE_COMMIT_MS = 5000
  const nodeNamePreviewText = (nodeName: string, currentState: UiState) => {
    if (nodeName.trim() === currentState.nodeName.trim()) {
      return currentState.selfMagicDnsName
        ? `Shared as ${currentState.selfMagicDnsName}`
        : 'Shared name has no DNS-safe .nvpn label yet.'
    }

    const preview = nodeNameDnsPreview(nodeName, currentState.magicDnsSuffix)
    return preview ? `Will share as ${preview}` : 'Shared name has no DNS-safe .nvpn label yet.'
  }

  $: serviceInstallRecommended = !!state?.serviceSupported && !state.serviceInstalled
  $: serviceEnableRecommended =
    !!state?.serviceEnablementSupported && !!state?.serviceInstalled && !!state?.serviceDisabled
  $: serviceRepairPromptRecommended = serviceRepairRecommended(error, state)
  $: serviceRepairRetryAfterInstall = serviceRepairRetryRecommended(error)
  $: serviceSetupRequired = serviceInstallRecommended && !state?.daemonRunning
  $: vpnControlSupported = !!state?.vpnSessionControlSupported
  $: cliInstallSupported = !!state?.cliInstallSupported
  $: startupSettingsSupported = !!state?.startupSettingsSupported
  $: trayBehaviorSupported = !!state?.trayBehaviorSupported
  $: {
    if (
      state &&
      serviceRepairPromptRecommended &&
      !actionInFlight &&
      !refreshInFlight &&
      !serviceRepairPromptInFlight &&
      !appDisposed
    ) {
      void maybePromptForServiceRepair()
    }
  }

  function syncLanPairingCountdown() {
    const now = Date.now()
    lanPairingDeadlineMs = lanPairingDeadlineFromSnapshot(
      lanPairingDeadlineMs,
      !!state?.lanPairingActive,
      state?.lanPairingRemainingSecs ?? 0,
      now,
    )
    lanPairingDisplayRemainingSecs = remainingSecsFromDeadline(lanPairingDeadlineMs, now)
  }

  function tickLanPairingCountdown() {
    lanPairingDisplayRemainingSecs = remainingSecsFromDeadline(lanPairingDeadlineMs, Date.now())
  }

  $: if (state) {
    syncLanPairingCountdown()
  } else {
    lanPairingDeadlineMs = null
    lanPairingDisplayRemainingSecs = 0
  }

  async function refresh() {
    if (refreshInFlight || actionInFlight) {
      return
    }
    refreshInFlight = true
    try {
      state = await tick()
      initializeDraftsOnce()
      syncDraftsFromState()
    } catch (err) {
      error = String(err)
    } finally {
      refreshInFlight = false
    }
  }

  function currentServiceRepairPromptKey(currentState: UiState) {
    return `${currentState.appVersion}:${
      currentState.serviceBinaryVersion || currentState.daemonBinaryVersion || 'unknown'
    }`
  }

  async function maybePromptForServiceRepair() {
    if (
      !state ||
      !serviceRepairRecommended(error, state) ||
      serviceRepairPromptInFlight ||
      actionInFlight ||
      refreshInFlight
    ) {
      return
    }

    const promptKey = currentServiceRepairPromptKey(state)
    if (serviceRepairPromptShownFor === promptKey) {
      return
    }
    serviceRepairPromptShownFor = promptKey

    if (typeof window.confirm !== 'function') {
      return
    }

    serviceRepairPromptInFlight = true
    try {
      if (
        window.confirm(
          'Background service is out of date. Reinstall it now so this app version can control the VPN?'
        )
      ) {
        await onRepairSystemService(false)
      }
    } finally {
      serviceRepairPromptInFlight = false
    }
  }

  async function ensureStateLoaded() {
    if (!state) {
      await refresh()
    }
    return state
  }

  async function handleAppDeepLink(url: string) {
    const normalized = url.trim()
    if (!normalized || processedDeepLinks.has(normalized)) {
      return
    }
    processedDeepLinks.add(normalized)

    const action = parseAppDeepLink(normalized)
    if (!action) {
      return
    }

    if (action.type === 'invite') {
      await runAction(() => importNetworkInvite(action.invite))
      return
    }

    if (action.type === 'tick') {
      await refresh()
      return
    }

    const current = await ensureStateLoaded()
    const network = current ? activeNetwork(current) : null
    if (!network) {
      return
    }

    if (action.type === 'request-join') {
      await runAction(() => requestNetworkJoin(network.id))
      return
    }

    await runAction(() => acceptJoinRequest(network.id, action.requesterNpub))
  }

  async function initializeDeepLinkHandling() {
    if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) {
      return
    }

    try {
      deepLinkUnlisten = await listen('deep-link://new-url', async (event) => {
        const urls = Array.isArray(event.payload) ? event.payload : []
        for (const url of urls) {
          if (typeof url === 'string') {
            await handleAppDeepLink(url)
          }
        }
      })

      const current = await invoke<string[] | null>('plugin:deep-link|get_current')
      if (!Array.isArray(current)) {
        return
      }
      for (const url of current) {
        if (typeof url === 'string') {
          await handleAppDeepLink(url)
        }
      }
    } catch (err) {
      console.error('Failed to initialize deep-link handling', err)
    }
  }

  function markBootReady() {
    if (bootReadyDispatched) {
      return
    }

    bootReadyDispatched = true
    dispatchBootReady(window)
  }

  function initializeDraftsOnce() {
    if (!state || draftsInitialized) {
      return
    }

    nodeNameDraft = state.nodeName
    endpointDraft = state.endpoint
    tunnelIpDraft = state.tunnelIp
    listenPortDraft = String(state.listenPort)
    exitNodeDraft = state.exitNode
    advertisedRoutesDraft = state.advertisedRoutes.join(', ')
    magicDnsSuffixDraft = state.magicDnsSuffix
    draftsInitialized = true
    syncDraftsFromState()
  }

  function syncDraftsFromState() {
    if (!state) {
      networkNameDrafts = {}
      networkIdDrafts = {}
      networkIdErrors = {}
      participantAliasDrafts = {}
      return
    }

    const nextNetworkNames: Record<string, string> = {}
    const nextNetworkIds: Record<string, string> = {}
    const nextParticipantInput: Record<string, string> = {}
    const nextParticipantAddAlias: Record<string, string> = {}
    const nextParticipantAliases: Record<string, string> = {}

    for (const network of state.networks) {
      const nameDebounceKey = `network-name-${network.id}`
      const meshIdDebounceKey = `network-id-${network.id}`
      nextNetworkNames[network.id] = debouncers.has(nameDebounceKey)
        ? (networkNameDrafts[network.id] ?? network.name)
        : network.name
      nextNetworkIds[network.id] = debouncers.has(meshIdDebounceKey) || !!networkIdErrors[network.id]
        ? (networkIdDrafts[network.id] ?? formatMeshIdForDisplay(network.networkId))
        : formatMeshIdForDisplay(network.networkId)

      nextParticipantInput[network.id] = participantInputDrafts[network.id] ?? ''
      nextParticipantAddAlias[network.id] = participantAddAliasDrafts[network.id] ?? ''

      for (const participant of network.participants) {
        const aliasDebounceKey = `alias-${participant.pubkeyHex}`
        nextParticipantAliases[participant.pubkeyHex] = debouncers.has(aliasDebounceKey)
          ? (participantAliasDrafts[participant.pubkeyHex] ?? participant.magicDnsAlias)
          : participant.magicDnsAlias
      }
    }

    networkNameDrafts = nextNetworkNames
    networkIdDrafts = nextNetworkIds
    participantInputDrafts = nextParticipantInput
    participantAddAliasDrafts = nextParticipantAddAlias
    participantAliasDrafts = nextParticipantAliases

    if (!debouncers.has('magicDnsSuffix')) {
      magicDnsSuffixDraft = state.magicDnsSuffix
    }
    exitNodeDraft = state.exitNode
    if (state.advertisedRoutes.length > 0) {
      showAdvancedRoutes = true
    }
    if (!debouncers.has('advertisedRoutes')) {
      advertisedRoutesDraft = state.advertisedRoutes.join(', ')
    }
  }

  function clearDebounce(key: string) {
    const existing = debouncers.get(key)
    if (existing) {
      window.clearTimeout(existing)
      debouncers.delete(key)
    }
  }

  function debounce(key: string, fn: () => Promise<void>, delay = 450) {
    clearDebounce(key)

    const timer = window.setTimeout(async () => {
      debouncers.delete(key)
      await fn()
    }, delay)

    debouncers.set(key, timer)
  }

  function networkMeshIdDebounceKey(networkId: string) {
    return `network-id-${networkId}`
  }

  function currentNetworkMeshId(networkId: string) {
    return state?.networks.find((network) => network.id === networkId)?.networkId ?? null
  }

  function setNetworkMeshIdError(networkId: string, message: string) {
    if (message) {
      networkIdErrors = {
        ...networkIdErrors,
        [networkId]: message,
      }
      return
    }

    if (!(networkId in networkIdErrors)) {
      return
    }

    const nextErrors = { ...networkIdErrors }
    delete nextErrors[networkId]
    networkIdErrors = nextErrors
  }

  function meshIdDraftError(networkId: string) {
    return networkIdErrors[networkId] ?? ''
  }

  function meshIdHelperText(networkId: string, currentMeshId: string) {
    const errorMessage = meshIdDraftError(networkId)
    if (errorMessage) {
      return errorMessage
    }
    return 'Best for new IDs: letters or numbers in 4-character groups, like abcd-efgh-ijkl.'
  }

  async function commitNetworkMeshId(networkId: string, value: string) {
    const debounceKey = networkMeshIdDebounceKey(networkId)
    clearDebounce(debounceKey)

    const currentMeshId = currentNetworkMeshId(networkId)
    if (!currentMeshId) {
      return
    }

    const trimmed = value.trim()
    if (!trimmed) {
      setNetworkMeshIdError(networkId, '')
      networkIdDrafts = {
        ...networkIdDrafts,
        [networkId]: formatMeshIdForDisplay(currentMeshId),
      }
      return
    }

    const validationError = validateMeshIdInput(trimmed, currentMeshId)
    if (validationError) {
      setNetworkMeshIdError(networkId, validationError)
      return
    }

    const normalized = canonicalizeMeshIdInput(trimmed, currentMeshId)
    if (normalized === currentMeshId) {
      setNetworkMeshIdError(networkId, '')
      networkIdDrafts = {
        ...networkIdDrafts,
        [networkId]: formatMeshIdForDisplay(currentMeshId),
      }
      return
    }

    setNetworkMeshIdError(networkId, '')
    await runAction(() => setNetworkMeshId(networkId, normalized))
  }

  async function runAction(action: () => Promise<UiState>) {
    if (actionInFlight) {
      return
    }
    actionInFlight = true
    try {
      state = await action()
      error = ''
      initializeDraftsOnce()
      syncDraftsFromState()
    } catch (err) {
      error = String(err)
      cliActionStatus = ''
      serviceActionStatus = ''
      try {
        state = await tick()
        initializeDraftsOnce()
        syncDraftsFromState()
      } catch {
        // Keep the original action error if state refresh also fails.
      }
    } finally {
      actionInFlight = false
    }
  }

  async function onToggleSession() {
    if (!state) {
      return
    }

    if (serviceSetupRequired && !state.sessionActive) {
      await onInstallSystemService(true)
      return
    }

    if (serviceEnableRecommended && !state.sessionActive) {
      await onEnableSystemService(true)
      return
    }

    await runAction(state.sessionActive ? disconnectSession : connectSession)
  }

  async function onInstallCli() {
    await runAction(installCli)
    if (!error) {
      cliActionStatus = 'CLI installed in PATH (/usr/local/bin/nvpn)'
    }
  }

  async function onUninstallCli() {
    await runAction(uninstallCli)
    if (!error) {
      cliActionStatus = 'CLI removed from PATH (/usr/local/bin/nvpn)'
    }
  }

  async function onInstallSystemService(connectAfter = false) {
    const wasInstalled = !!state?.serviceInstalled
    await runAction(installSystemService)
    if (!error) {
      serviceActionStatus = wasInstalled
        ? 'System service reinstalled and started'
        : 'System service installed and started'
    } else if (!wasInstalled && state?.serviceInstalled) {
      error = ''
      serviceActionStatus = state.serviceRunning
        ? 'System service installed and started'
        : 'System service installed'
    }
    if (connectAfter && !error && state && !state.sessionActive) {
      await runAction(connectSession)
      if (!error) {
        serviceActionStatus = state.sessionActive
          ? wasInstalled
            ? 'System service reinstalled and VPN started'
            : 'System service installed and VPN started'
          : wasInstalled
            ? 'System service reinstalled'
            : 'System service installed'
      }
    }
  }

  async function onRepairSystemService(connectAfter = false) {
    await onInstallSystemService(connectAfter)
  }

  async function onEnableSystemService(connectAfter = false) {
    const wasDisabled = !!state?.serviceDisabled
    await runAction(enableSystemService)
    if (!error) {
      serviceActionStatus = 'System service enabled and started'
    } else if (wasDisabled && state && !state.serviceDisabled) {
      error = ''
      serviceActionStatus = state.serviceRunning
        ? 'System service enabled and started'
        : 'System service enabled'
    }
    if (connectAfter && !error && state && !state.sessionActive) {
      await runAction(connectSession)
    }
  }

  async function onDisableSystemService() {
    const wasEnabled = !!state?.serviceInstalled && !state?.serviceDisabled
    await runAction(disableSystemService)
    if (!error) {
      serviceActionStatus = 'System service disabled'
    } else if (wasEnabled && state?.serviceDisabled) {
      error = ''
      serviceActionStatus = 'System service disabled'
    }
  }

  async function onUninstallSystemService() {
    const wasInstalled = !!state?.serviceInstalled
    await runAction(uninstallSystemService)
    if (!error) {
      serviceActionStatus = 'System service removed'
    } else if (wasInstalled && state && !state.serviceInstalled) {
      error = ''
      serviceActionStatus = 'System service removed'
    }
  }

  async function onAddNetwork() {
    const name = newNetworkName.trim()
    await runAction(() => addNetwork(name))
    newNetworkName = ''
  }

  function onNetworkNameInput(networkId: string, value: string) {
    networkNameDrafts = {
      ...networkNameDrafts,
      [networkId]: value,
    }

    debounce(`network-name-${networkId}`, async () => {
      await runAction(() => renameNetwork(networkId, value))
    }, 500)
  }

  function onNetworkMeshIdInput(networkId: string, value: string) {
    networkIdDrafts = {
      ...networkIdDrafts,
      [networkId]: value,
    }

    const currentMeshId = currentNetworkMeshId(networkId)
    if (!currentMeshId) {
      return
    }

    const normalized = value.trim()
    const debounceKey = networkMeshIdDebounceKey(networkId)
    const validationError = validateMeshIdInput(normalized, currentMeshId)
    setNetworkMeshIdError(networkId, validationError)

    if (validationError) {
      clearDebounce(debounceKey)
      return
    }

    const canonical = canonicalizeMeshIdInput(normalized, currentMeshId)
    if (!canonical || canonical === currentMeshId) {
      clearDebounce(debounceKey)
      return
    }

    debounce(debounceKey, () => commitNetworkMeshId(networkId, value), NETWORK_MESH_ID_IDLE_COMMIT_MS)
  }

  async function onAddParticipant(networkId: string) {
    const npub = participantInputDrafts[networkId]?.trim() || ''
    const alias = participantAddAliasDrafts[networkId]?.trim() || ''
    if (!npub) {
      return
    }

    await runAction(() => addParticipant(networkId, npub, alias))
    participantInputDrafts = {
      ...participantInputDrafts,
      [networkId]: '',
    }
    participantAddAliasDrafts = {
      ...participantAddAliasDrafts,
      [networkId]: '',
    }
  }

  async function onToggleAdmin(networkId: string, participant: ParticipantView) {
    if (participant.isAdmin) {
      await runAction(() => removeAdmin(networkId, participant.npub))
      return
    }
    await runAction(() => addAdmin(networkId, participant.npub))
  }

  async function onJoinLanPeer(invite: string) {
    await importInviteCode(invite)
  }

  type InviteImportOptions = {
    autoConnectOnSuccess?: boolean
  }

  async function ensureSessionActiveAfterInviteImport() {
    if (!state || !state.vpnSessionControlSupported || state.sessionActive) {
      return
    }

    if (serviceSetupRequired) {
      await onInstallSystemService(true)
      return
    }

    if (serviceEnableRecommended) {
      await onEnableSystemService(true)
      return
    }

    await runAction(connectSession)
  }

  async function importInviteCode(
    invite: string,
    options: InviteImportOptions = {},
  ) {
    const normalized = invite.trim()
    if (!normalized) {
      return false
    }

    await runAction(() => importNetworkInvite(normalized))
    if (!error && options.autoConnectOnSuccess) {
      await ensureSessionActiveAfterInviteImport()
    }
    return !error
  }

  async function onRequestNetworkJoin(networkId: string) {
    await runAction(() => requestNetworkJoin(networkId))
  }

  async function onAcceptJoinRequest(networkId: string, requesterNpub: string) {
    await runAction(() => acceptJoinRequest(networkId, requesterNpub))
  }

  async function onToggleJoinRequests(networkId: string, enabled: boolean) {
    await runAction(() => setNetworkJoinRequestsEnabled(networkId, enabled))
  }

  async function onStartLanPairing() {
    await runAction(() => startLanPairing())
  }

  async function onStopLanPairing() {
    await runAction(() => stopLanPairing())
  }

  async function onAddRelay() {
    const relay = relayInput.trim()
    if (!relay) {
      return
    }

    await runAction(() => addRelay(relay))
    relayInput = ''
  }

  function onAdvertisedRoutesInput(value: string) {
    advertisedRoutesDraft = value
    debounce('advertisedRoutes', () => onUpdateSettings({ advertisedRoutes: advertisedRoutesDraft }))
  }

  async function onUpdateSettings(patch: SettingsPatch) {
    await runAction(() => updateSettings(patch))
  }

  async function onSelectExitNode(npub: string) {
    exitNodeDraft = npub
    await onUpdateSettings({ exitNode: npub })
  }

  function onParticipantAliasInput(
    participantNpub: string,
    participantHex: string,
    value: string,
  ) {
    participantAliasDrafts = {
      ...participantAliasDrafts,
      [participantHex]: value,
    }

    debounce(
      `alias-${participantHex}`,
      async () => {
        await runAction(() => setParticipantAlias(participantNpub, value))
      },
      500,
    )
  }

  async function refreshAutostart() {
    if (!state) {
      autostartReady = true
      return
    }

    if (!state.startupSettingsSupported) {
      autostartReady = true
      return
    }

    const runtimeEnabled = await isAutostartEnabled()
    if (runtimeEnabled !== state.launchOnStartup) {
      const ok = await setAutostartEnabled(state.launchOnStartup)
      // Startup sync can run in environments where autostart cannot be managed
      // (for example the Linux Tauri-driver container), so avoid surfacing a
      // boot-time banner unless the user explicitly changed the setting.
      if (!ok) {
        autostartReady = true
        return
      }
    }

    autostartReady = true
  }

  async function onToggleAutostart(enabled: boolean) {
    if (!state || !state.startupSettingsSupported) {
      return
    }

    const previous = state.launchOnStartup
    autostartUpdating = true
    await onUpdateSettings({ launchOnStartup: enabled })
    const ok = await setAutostartEnabled(enabled)

    if (!ok) {
      error = 'Failed to update autostart setting'
      await onUpdateSettings({ launchOnStartup: previous })
    } else {
      await refreshAutostart()
    }

    autostartUpdating = false
  }

  async function copyText(
    value: string,
    kind: 'pubkey' | 'meshId' | 'invite' | 'peerNpub',
    peerNpub: string | null = null,
  ) {
    try {
      await navigator.clipboard.writeText(value)
      copiedValue = kind
      copiedPeerNpub = kind === 'peerNpub' ? (peerNpub ?? value) : null
      if (copiedHandle) {
        window.clearTimeout(copiedHandle)
      }
      copiedHandle = window.setTimeout(() => {
        copiedValue = null
        copiedPeerNpub = null
        copiedHandle = null
      }, 2000)
    } catch {
      error = 'Clipboard copy failed'
    }
  }

  async function copyPubkey() {
    if (!state) {
      return
    }

    await copyText(state.ownNpub, 'pubkey')
  }

  async function copyPeerNpub(npub: string) {
    await copyText(npub, 'peerNpub', npub)
  }

  async function copyMeshId() {
    if (!state) {
      return
    }

    const network = activeNetwork(state)
    const draftMeshId = networkIdDrafts[network.id] ?? formatMeshIdForDisplay(network.networkId)
    const rawMeshId = meshIdDraftError(network.id)
      ? network.networkId
      : canonicalizeMeshIdInput(draftMeshId, network.networkId)
    await copyText(rawMeshId, 'meshId')
  }

  async function copyInvite() {
    if (!state?.activeNetworkInvite) {
      return
    }

    await copyText(state.activeNetworkInvite, 'invite')
  }

  onMount(() => {
    lanPairingTickHandle = window.setInterval(tickLanPairingCountdown, 1000)

    void (async () => {
      await waitForNextPaint(window)
      if (appDisposed) {
        return
      }

      await refresh()
      if (appDisposed) {
        return
      }

      await initializeDeepLinkHandling()
      if (appDisposed) {
        return
      }

      markBootReady()
      await refreshAutostart()
      if (appDisposed) {
        return
      }

      pollHandle = window.setInterval(refresh, 1500)
    })()
  })

  onDestroy(() => {
    appDisposed = true
    if (pollHandle) {
      window.clearInterval(pollHandle)
    }
    if (lanPairingTickHandle) {
      window.clearInterval(lanPairingTickHandle)
    }
    if (copiedHandle) {
      window.clearTimeout(copiedHandle)
    }
    if (deepLinkUnlisten) {
      deepLinkUnlisten()
    }
    for (const timer of debouncers.values()) {
      window.clearTimeout(timer)
    }
  })
</script>

<main class="app-shell">
  <div class="drag-padding drag-padding-top" data-tauri-drag-region aria-hidden="true"></div>
  <div class="drag-padding drag-padding-left" data-tauri-drag-region aria-hidden="true"></div>
  <div class="drag-padding drag-padding-right" data-tauri-drag-region aria-hidden="true"></div>
  <div class="drag-padding drag-padding-bottom" data-tauri-drag-region aria-hidden="true"></div>

  <header class="window-chrome" data-tauri-drag-region>
    <div class="window-title" data-testid="window-title">Nostr VPN</div>
  </header>

  <section class="identity-card panel hero-card">
    {#if state}
      {@const activeNetworkView = activeNetwork(state)}
      <div class="row hero-row">
        <div class="hero-copy">
          <div class="panel-kicker">Status</div>
          <div class="row hero-title-row">
            <h1 data-testid="active-network-title">{activeNetworkView.name}</h1>
            {#if activeNetworkView.localIsAdmin}
              <span class="badge ok" data-testid="active-network-admin-badge">Admin</span>
            {/if}
            <span class={`badge ${heroStateBadgeClass(state)}`}>
              {heroBadgeText(state)}
            </span>
          </div>
          <div class="hero-subtitle">{heroSubtext(state)}</div>
        </div>
        {#if vpnControlSupported && !serviceSetupRequired}
          <button
            class={`session-switch ${state.sessionActive ? 'on' : 'off'}`}
            role="switch"
            aria-checked={state.sessionActive}
            aria-label="Toggle VPN session"
            data-testid="session-toggle"
            on:click={onToggleSession}
          >
            <span class="session-switch-track" aria-hidden="true">
              <span class="session-switch-thumb"></span>
            </span>
            <span class="session-switch-label">VPN {state.sessionActive ? 'On' : 'Off'}</span>
          </button>
        {/if}
      </div>

      <div class="hero-stats-grid">
        <div class="hero-stat-card" data-testid="hero-identity-card">
          <div class="panel-kicker">Identity</div>
          <div class="hero-identity-row">
            <div class="copy-value hero-copy-value" data-testid="pubkey">
              {state.ownNpub}
            </div>
            <button
              class="btn icon-btn hero-copy-icon-btn"
              type="button"
              aria-label="Copy npub"
              title="Copy npub"
              data-testid="copy-pubkey"
              on:click={copyPubkey}
            >
              <span class="copy-icon" aria-hidden="true">
                {#if copiedValue === 'pubkey'}
                  <Check size={16} strokeWidth={2.3} />
                {:else}
                  <Copy size={16} strokeWidth={2.2} />
                {/if}
              </span>
            </button>
          </div>
        </div>

        <div class="hero-stat-card hero-device-card">
          <div class="panel-kicker">This device</div>
          <input
            class="text-input hero-device-name-input"
            data-testid="node-name-input"
            bind:value={nodeNameDraft}
            on:input={() => debounce('nodeName', () => onUpdateSettings({ nodeName: nodeNameDraft }))}
          />
          <div class="config-path hero-device-preview">{nodeNamePreviewText(nodeNameDraft, state)}</div>
          <div class="config-path">{state.tunnelIp} • {state.endpoint}</div>
        </div>
      </div>

      <div class="row status-row">
        {#if vpnControlSupported}
          <span class={`badge ${state.daemonRunning ? 'ok' : 'bad'}`}>
            Daemon {state.daemonRunning ? 'Running' : 'Stopped'}
          </span>
          <span class={`badge ${state.sessionActive ? 'ok' : 'bad'}`}>
            VPN {state.sessionActive ? 'On' : 'Off'}
          </span>
          <span class={`badge ${state.relayConnected ? 'ok' : 'muted'}`}>
            Relays {state.relayConnected ? 'Connected' : 'Disconnected'}
          </span>
          <span class="badge muted" data-testid="mesh-badge">
            Mesh {state.connectedPeerCount}/{state.expectedPeerCount}
          </span>
        {:else}
          <span class="badge muted">Platform {platformLabel(state.platform)}</span>
          <span class="badge muted">Config editing enabled</span>
          <span class="badge muted">Tunnel control unavailable</span>
        {/if}
      </div>
      {#if heroDetailText(state)}
        <div class="identity-status" data-testid="session-status-text">
          {heroDetailText(state)}
        </div>
      {/if}
    {:else}
      <div class="panel-kicker">Loading</div>
      <div class="row hero-title-row">
        <h1>Starting Nostr VPN</h1>
      </div>
      <div class="hero-subtitle">Loading config, daemon state, and local mesh status.</div>
    {/if}
  </section>

  {#if serviceRepairErrorText(error, state)}
    <section class="panel error">{serviceRepairErrorText(error, state)}</section>
  {/if}

  {#if state}
    {@const activeNetworkView = activeNetwork(state)}

    {#if serviceInstallRecommended || serviceEnableRecommended || serviceRepairPromptRecommended}
      <ServiceActionPanel
        {state}
        {serviceActionStatus}
        {serviceEnableRecommended}
        {serviceRepairPromptRecommended}
        {serviceRepairRetryAfterInstall}
        {serviceSetupRequired}
        {onDisableSystemService}
        {onEnableSystemService}
        {onInstallSystemService}
        {onRepairSystemService}
        {onUninstallSystemService}
      />
    {/if}

    <section class="panel spotlight-panel">
      <div class="section-title-row">
        <div>
          <div class="panel-kicker">Active network</div>
          <h2>{activeNetworkView.name}</h2>
        </div>
        <div class="section-meta">
          {onlineDeviceSummary(activeNetworkView.onlineCount, activeNetworkView.expectedCount)}
        </div>
      </div>

      <div class="spotlight-meta-grid">
        <div class="spotlight-meta-card spotlight-profile-card">
          <div class="panel-kicker">Profile</div>
          <div class="spotlight-profile-fields">
            <label class="field-label" for={`active-network-name-${activeNetworkView.id}`}>Name</label>
            <input
              id={`active-network-name-${activeNetworkView.id}`}
              class="text-input active-network-name-input"
              data-testid="network-name-input"
              value={networkNameDrafts[activeNetworkView.id] ?? activeNetworkView.name}
              on:input={(event) =>
                onNetworkNameInput(activeNetworkView.id, (event.currentTarget as HTMLInputElement).value)}
            />
            <label class="field-label" for={`active-network-mesh-${activeNetworkView.id}`}>Mesh ID</label>
            <div class="inline-copy-field">
              <input
                id={`active-network-mesh-${activeNetworkView.id}`}
                class={`text-input network-mesh-id-input ${meshIdDraftError(activeNetworkView.id) ? 'text-input-invalid' : ''}`}
                data-testid="active-network-mesh-id-input"
                value={formatMeshIdDraftForDisplay(
                  networkIdDrafts[activeNetworkView.id] ?? '',
                  activeNetworkView.networkId,
                )}
                on:input={(event) =>
                  onNetworkMeshIdInput(activeNetworkView.id, (event.currentTarget as HTMLInputElement).value)}
                on:blur={(event) =>
                  commitNetworkMeshId(activeNetworkView.id, (event.currentTarget as HTMLInputElement).value)}
                on:keydown={(event) =>
                  event.key === 'Enter' &&
                  commitNetworkMeshId(activeNetworkView.id, (event.currentTarget as HTMLInputElement).value)}
              />
              <button class="btn copy-btn" data-testid="copy-mesh-id" on:click={copyMeshId}>
                <span class="copy-icon" aria-hidden="true">
                  {#if copiedValue === 'meshId'}
                    <Check size={16} strokeWidth={2.3} />
                  {:else}
                    <Copy size={16} strokeWidth={2.2} />
                  {/if}
                </span>
                <span>{copiedValue === 'meshId' ? 'Copied' : 'Copy Mesh ID'}</span>
              </button>
            </div>
            <div class={`config-path ${meshIdDraftError(activeNetworkView.id) ? 'mesh-id-note-error' : ''}`}>
              {meshIdHelperText(activeNetworkView.id, activeNetworkView.networkId)}
            </div>
          </div>
          <div class="config-path">{networkPeerSummary(activeNetworkView)}</div>
          <div class="config-path">
            Stable identifier used for tunnel addressing and matching the right mesh.
          </div>
        </div>
        <div class="spotlight-meta-card spotlight-share-card">
          <div class="panel-kicker">Join & share</div>
          <div class="spotlight-meta-value">Copy, scan, or pair</div>
          <div class="config-path">
            Includes the Mesh ID, your npub, and the relay list for {activeNetworkView.name}.
          </div>
          <div class="config-path" data-testid="network-admin-summary">
            {networkAdminSummary(activeNetworkView)}
          </div>
          <label class="toggle-row">
            <input
              type="checkbox"
              checked={activeNetworkView.joinRequestsEnabled}
              disabled={!activeNetworkView.localIsAdmin}
              on:change={(event) =>
                onToggleJoinRequests(
                  activeNetworkView.id,
                  (event.currentTarget as HTMLInputElement).checked,
                )}
            />
            <div>Listen for join requests</div>
          </label>
          <div class="config-path">
            Join requests from invite holders will appear here.
          </div>
          {#if activeNetworkView.inboundJoinRequests.length > 0}
            <div class="lan-title">Pending join requests</div>
            <div class="stack rows">
              {#each activeNetworkView.inboundJoinRequests as request}
                <div class="item-row" data-testid="join-request-row">
                  <div class="item-main">
                    <div class="item-title">
                      {request.requesterNodeName || 'Pending device'}
                    </div>
                    <div class="peer-npub-row">
                      <div class="peer-npub-text">{request.requesterNpub}</div>
                      <button
                        class="btn ghost icon-btn peer-npub-copy-btn"
                        type="button"
                        aria-label="Copy peer npub"
                        title="Copy peer npub"
                        data-testid="copy-peer-npub"
                        on:click={() => copyPeerNpub(request.requesterNpub)}
                      >
                        <span class="copy-icon" aria-hidden="true">
                          {#if copiedValue === 'peerNpub' && copiedPeerNpub === request.requesterNpub}
                            <Check size={16} strokeWidth={2.3} />
                          {:else}
                            <Copy size={16} strokeWidth={2.2} />
                          {/if}
                        </span>
                      </button>
                    </div>
                    <div class="item-sub">
                      requested {request.requestedAtText}
                    </div>
                  </div>
                  <button
                    class="btn"
                    data-testid="accept-join-request"
                    disabled={!activeNetworkView.localIsAdmin}
                    on:click={() => onAcceptJoinRequest(activeNetworkView.id, request.requesterNpub)}
                  >
                    Accept
                  </button>
                </div>
              {/each}
            </div>
          {/if}
          <InviteShareSection
            {state}
            {activeNetworkView}
            {participantInputDrafts}
            {participantAddAliasDrafts}
            {copiedValue}
            {copiedPeerNpub}
            {lanPairingDisplayRemainingSecs}
            {formatCountdown}
            {copyInvite}
            {copyPeerNpub}
            {onStartLanPairing}
            {onStopLanPairing}
            {onJoinLanPeer}
            {onRequestNetworkJoin}
            {onAddParticipant}
            {lanPairingHelpText}
            onImportInviteCode={importInviteCode}
          />
        </div>
      </div>

      {#if activeNetworkView.participants.length === 0}
        <div class="item-row network-empty-state">
          <div class="item-main">
            <div class="item-title">No devices yet</div>
            <div class="item-sub">Import an invite, start LAN pairing, or add a participant npub to start building the active mesh.</div>
          </div>
        </div>
      {:else}
        <div class="stack rows">
          {#each activeNetworkView.participants as participant}
            <div class="item-row" data-testid="participant-row">
              <div class="item-main">
                <div class="peer-npub-row">
                  <div class="peer-npub-text" data-testid="participant-npub">{participant.npub}</div>
                  <button
                    class="btn ghost icon-btn peer-npub-copy-btn"
                    type="button"
                    aria-label="Copy peer npub"
                    title="Copy peer npub"
                    data-testid="copy-peer-npub"
                    on:click={() => copyPeerNpub(participant.npub)}
                  >
                    <span class="copy-icon" aria-hidden="true">
                      {#if copiedValue === 'peerNpub' && copiedPeerNpub === participant.npub}
                        <Check size={16} strokeWidth={2.3} />
                      {:else}
                        <Copy size={16} strokeWidth={2.2} />
                      {/if}
                    </span>
                  </button>
                </div>
                <div class="row alias-row">
                  <input
                    class="text-input alias-input"
                    value={participantAliasDrafts[participant.pubkeyHex] ?? participant.magicDnsAlias}
                    data-testid="participant-alias-input"
                    on:input={(event) =>
                      onParticipantAliasInput(
                        participant.npub,
                        participant.pubkeyHex,
                        (event.currentTarget as HTMLInputElement).value,
                      )}
                  />
                  {#if state.magicDnsSuffix}
                    <span class="alias-suffix">.{state.magicDnsSuffix}</span>
                  {/if}
                </div>
                <div class="item-sub" data-testid="participant-status-text">
                  {participant.magicDnsName || participant.magicDnsAlias || 'No alias'} | {participant.statusText} | {participant.lastSignalText} | {participant.tunnelIp}
                  | {participantTrafficText(participant)}
                  {#if participant.relayPathActive && participant.runtimeEndpoint}
                    | relay {participant.runtimeEndpoint}
                  {/if}
                  {#if participant.advertisedRoutes.length > 0}
                    | routes {participant.advertisedRoutes.join(', ')}
                  {/if}
                </div>
              </div>
              <div class="participant-badges">
                <span
                  class={`badge participant-badge ${participantBadgeClass(participant.state)}`}
                  data-testid="participant-state"
                >
                  {participantTransportBadgeText(participant)}
                </span>
                <span
                  class={`badge participant-badge ${participantBadgeClass(participant.presenceState)}`}
                  data-testid="participant-presence-state"
                >
                  {participantPresenceBadgeText(participant)}
                </span>
                {#if participant.isAdmin}
                  <span class="badge participant-badge ok" data-testid="participant-admin-badge">
                    Admin
                  </span>
                {/if}
                {#if participant.relayPathActive}
                  <span class="badge participant-badge warn">Relay fallback</span>
                {/if}
                {#if participant.offersExitNode}
                  <span class="badge participant-badge warn">Private exit</span>
                {/if}
                {#if state.exitNode === participant.npub}
                  <span class="badge participant-badge ok">Selected exit</span>
                {/if}
              </div>
              {#if activeNetworkView.localIsAdmin}
                <button
                  class="btn ghost"
                  data-testid="participant-toggle-admin"
                  on:click={() => onToggleAdmin(activeNetworkView.id, participant)}
                >
                  {participant.isAdmin ? 'Remove admin' : 'Make admin'}
                </button>
              {/if}
              <button
                class="btn ghost icon-btn"
                data-testid="participant-remove"
                title="Delete participant"
                aria-label="Delete participant"
                disabled={!activeNetworkView.localIsAdmin}
                on:click={() => runAction(() => removeParticipant(activeNetworkView.id, participant.npub))}
              >
                <Trash2 size={16} strokeWidth={2.2} />
              </button>
            </div>
          {/each}
        </div>
      {/if}

    </section>

    <RoutingPanel
      {state}
      {advertisedRoutesDraft}
      bind:exitNodeSearch
      {onAdvertisedRoutesInput}
      {onUpdateSettings}
      {onSelectExitNode}
    />

    <SavedNetworksPanel
      bind:newNetworkName
      {state}
      inactiveNetworks={inactiveNetworks(state)}
      {networkNameDrafts}
      {networkIdDrafts}
      {participantInputDrafts}
      {participantAddAliasDrafts}
      {participantAliasDrafts}
      {copiedValue}
      {copiedPeerNpub}
      formatMeshIdForDisplay={formatMeshIdForDisplay}
      formatMeshIdDraftForDisplay={formatMeshIdDraftForDisplay}
      networkPeerSummary={networkPeerSummary}
      networkAdminSummary={networkAdminSummary}
      meshIdDraftError={meshIdDraftError}
      meshIdHelperText={meshIdHelperText}
      onNetworkNameInput={onNetworkNameInput}
      onNetworkMeshIdInput={onNetworkMeshIdInput}
      commitNetworkMeshId={commitNetworkMeshId}
      onToggleJoinRequests={onToggleJoinRequests}
      copyPeerNpub={copyPeerNpub}
      onAcceptJoinRequest={onAcceptJoinRequest}
      onAddParticipant={onAddParticipant}
      onAddNetwork={onAddNetwork}
      onRequestNetworkJoin={onRequestNetworkJoin}
      onRemoveParticipant={(networkId, npub) => runAction(() => removeParticipant(networkId, npub))}
      onParticipantAliasInput={onParticipantAliasInput}
      runAction={runAction}
      removeNetwork={removeNetwork}
      setNetworkEnabled={setNetworkEnabled}
    />
    <!-- {#if network.localIsAdmin}
    <span class="badge ok" data-testid="saved-network-admin-badge">
      Admin
    </span>
    -->

    <AdvancedPanels
      {state}
      {activeNetworkView}
      bind:relayInput
      {onAddRelay}
      onRemoveRelay={(relayUrl) => runAction(() => removeRelay(relayUrl))}
      {onUpdateSettings}
    />

    <SystemPanel
      {state}
      {cliActionStatus}
      {autostartReady}
      {autostartUpdating}
      {cliInstallSupported}
      {startupSettingsSupported}
      {trayBehaviorSupported}
      {magicDnsSuffixDraft}
      {endpointDraft}
      {tunnelIpDraft}
      {listenPortDraft}
      {onInstallCli}
      {onUninstallCli}
      {onToggleAutostart}
      {onUpdateSettings}
      {debounce}
    />
  {/if}
</main>
