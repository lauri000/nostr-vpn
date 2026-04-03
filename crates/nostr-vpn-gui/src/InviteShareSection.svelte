<script lang="ts">
  import { onDestroy } from 'svelte'
  import jsQR from 'jsqr'
  import QRCode from 'qrcode'

  import { waitForNextPaint } from './lib/boot.js'
  import {
    buildInviteScanConstraintCandidates,
    openInviteScanStream,
  } from './lib/invite-scan.js'
  import { decodeInvitePayload, determineInviteImportTarget } from './lib/invite-code.js'
  import {
    inviteInviterParticipant,
    joinRequestButtonLabel,
    joinRequestStatusText,
    networkHasParticipant,
    short,
  } from './lib/app-view'
  import InviteSharePanel from './InviteSharePanel.svelte'
  import type { NetworkView, UiState } from './lib/types'

  type InviteImportOptions = {
    autoConnectOnSuccess?: boolean
  }

  export let state: UiState
  export let activeNetworkView: NetworkView
  export let copiedValue: 'pubkey' | 'meshId' | 'invite' | 'peerNpub' | null = null
  export let copiedPeerNpub: string | null = null
  export let participantInputDrafts: Record<string, string>
  export let participantAddAliasDrafts: Record<string, string>
  export let lanPairingDisplayRemainingSecs = 0
  export let lanPairingHelpText: (state: UiState) => string
  export let formatCountdown: (value: number) => string
  export let copyInvite: () => Promise<void>
  export let copyPeerNpub: (npub: string) => Promise<void>
  export let onStartLanPairing: () => Promise<void>
  export let onStopLanPairing: () => Promise<void>
  export let onJoinLanPeer: (invite: string) => Promise<void>
  export let onRequestNetworkJoin: (networkId: string) => Promise<void>
  export let onAddParticipant: (networkId: string) => Promise<void>
  export let onImportInviteCode: (
    invite: string,
    options?: InviteImportOptions,
  ) => Promise<boolean>

  let inviteQrDataUrl = ''
  let inviteQrError = ''
  let inviteQrSource = ''
  let inviteQrSequence = 0
  let inviteInputDraft = ''
  let inviteImportHandle: number | null = null
  let inviteImportPendingValue = ''
  let inviteImportLastAttemptedValue = ''
  let inviteScanInput: HTMLInputElement | null = null
  let inviteScanVideo: HTMLVideoElement | null = null
  let inviteScanCanvas: HTMLCanvasElement | null = null
  let inviteScanStream: MediaStream | null = null
  let inviteScanOpen = false
  let inviteScanBusy = false
  let inviteScanStatus = ''
  let inviteScanError = ''
  let inviteScanFrameHandle: number | null = null

  $: lanJoinCandidates = state.lanPeers.filter(
    (peer) => !networkHasParticipant(activeNetworkView, peer.npub),
  )

  $: {
    const invite = state.activeNetworkInvite ?? ''
    if (invite !== inviteQrSource) {
      inviteQrSource = invite
      void refreshInviteQr(invite)
    }
  }

  function ensureInviteScanCanvas() {
    if (!inviteScanCanvas) {
      inviteScanCanvas = document.createElement('canvas')
    }
    return inviteScanCanvas
  }

  function decodeInviteFromImageSource(
    source: CanvasImageSource,
    width: number,
    height: number,
  ) {
    if (!width || !height) {
      return null
    }

    const canvas = ensureInviteScanCanvas()
    canvas.width = width
    canvas.height = height
    const context = canvas.getContext('2d', { willReadFrequently: true })
    if (!context) {
      throw new Error('QR scanner could not read image pixels')
    }

    context.drawImage(source, 0, 0, width, height)
    const imageData = context.getImageData(0, 0, width, height)
    return jsQR(imageData.data, width, height, {
      inversionAttempts: 'attemptBoth',
    })?.data?.trim() || null
  }

  function describeInviteScanError(err: unknown) {
    const name =
      err && typeof err === 'object' && 'name' in err ? String((err as { name?: unknown }).name) : ''
    switch (name) {
      case 'NotAllowedError':
      case 'PermissionDeniedError':
        return 'Camera access was denied. You can still scan a saved QR image.'
      case 'NotFoundError':
      case 'DevicesNotFoundError':
        return 'No camera was found. You can still scan a saved QR image.'
      case 'NotReadableError':
      case 'TrackStartError':
        return 'The camera is busy in another app. Close it there or scan a saved QR image.'
      default:
        return `Live QR scanning failed: ${String(err)}`
    }
  }

  function stopInviteScan() {
    if (inviteScanFrameHandle !== null) {
      window.cancelAnimationFrame(inviteScanFrameHandle)
      inviteScanFrameHandle = null
    }
    if (inviteScanStream) {
      for (const track of inviteScanStream.getTracks()) {
        track.stop()
      }
      inviteScanStream = null
    }
    if (inviteScanVideo) {
      inviteScanVideo.pause()
      inviteScanVideo.srcObject = null
    }
    inviteScanBusy = false
    inviteScanOpen = false
  }

  function queueInviteScanFrame() {
    if (!inviteScanOpen) {
      return
    }

    inviteScanFrameHandle = window.requestAnimationFrame(() => {
      inviteScanFrameHandle = null
      void scanInviteVideoFrame()
    })
  }

  function buildInviteImportPrompt(invite: {
    networkName: string
    networkId: string
    inviterNpub: string
  }) {
    const lines = [`Import invite for "${invite.networkName}" from ${short(invite.inviterNpub, 18, 12)}?`]

    const importTarget = determineInviteImportTarget(
      state.networks,
      activeNetworkView.id,
      invite.networkId,
    )
    switch (importTarget.mode) {
      case 'existing':
        lines.push('This adds the scanned device to the matching network you already have.')
        break
      case 'reuse-active':
        lines.push('This reuses your current empty network slot and fills it from the invite.')
        break
      case 'create':
      default:
        lines.push('This creates a new network entry so your current network stays untouched.')
        break
    }

    lines.push('Press Cancel to fill the invite field instead of importing right away.')
    return lines.join('\n\n')
  }

  function clearInviteImportDebounce() {
    if (inviteImportHandle !== null) {
      window.clearTimeout(inviteImportHandle)
      inviteImportHandle = null
    }
  }

  async function importInviteCode(invite: string, options: InviteImportOptions = {}) {
    const normalized = invite.trim()
    if (!normalized) {
      return false
    }

    inviteInputDraft = normalized
    const succeeded = await onImportInviteCode(normalized, {
      autoConnectOnSuccess: options.autoConnectOnSuccess,
    })
    if (succeeded) {
      inviteImportLastAttemptedValue = ''
    }
    return succeeded
  }

  function scheduleInviteImportAttempt(invite: string) {
    const normalized = invite.trim()
    inviteImportPendingValue = normalized
    clearInviteImportDebounce()

    if (!normalized) {
      inviteImportLastAttemptedValue = ''
      return
    }

    inviteImportHandle = window.setTimeout(async () => {
      inviteImportHandle = null
      if (inviteImportBusy()) {
        scheduleInviteImportAttempt(inviteImportPendingValue)
        return
      }

      const pending = inviteImportPendingValue.trim()
      if (!pending || pending === inviteImportLastAttemptedValue) {
        return
      }

      inviteImportLastAttemptedValue = pending
      await importInviteCode(pending, {
        autoConnectOnSuccess: true,
      })
    }, 250)
  }

  function inviteImportBusy() {
    return inviteScanBusy
  }

  async function handleScannedInvite(invite: string) {
    const normalized = invite.trim()
    let parsed
    try {
      parsed = decodeInvitePayload(normalized)
    } catch (err) {
      throw new Error(`Scanned QR is not a valid Nostr VPN invite: ${String(err)}`)
    }

    inviteScanError = ''
    inviteScanStatus = ''

    if (typeof window.confirm === 'function' && !window.confirm(buildInviteImportPrompt(parsed))) {
      inviteInputDraft = normalized
      inviteScanStatus = 'Invite loaded into the field.'
      return
    }

    const imported = await importInviteCode(normalized, {
      autoConnectOnSuccess: true,
    })
    if (imported) {
      inviteScanStatus = `Imported ${parsed.networkName}.`
    }
  }

  async function scanInviteVideoFrame() {
    if (!inviteScanOpen || !inviteScanVideo) {
      return
    }

    if (
      inviteScanVideo.readyState < HTMLMediaElement.HAVE_CURRENT_DATA ||
      inviteScanVideo.videoWidth === 0 ||
      inviteScanVideo.videoHeight === 0
    ) {
      queueInviteScanFrame()
      return
    }

    const invite = decodeInviteFromImageSource(
      inviteScanVideo,
      inviteScanVideo.videoWidth,
      inviteScanVideo.videoHeight,
    )
    if (!invite) {
      queueInviteScanFrame()
      return
    }

    stopInviteScan()
    try {
      await handleScannedInvite(invite)
    } catch (err) {
      inviteScanError = String(err)
    }
  }

  async function refreshInviteQr(invite: string) {
    const sequence = ++inviteQrSequence
    if (!invite) {
      inviteQrDataUrl = ''
      inviteQrError = ''
      return
    }

    try {
      const dataUrl = await QRCode.toDataURL(invite, {
        errorCorrectionLevel: 'M',
        margin: 1,
        scale: 8,
        color: {
          dark: '#121926',
          light: '#ffffff',
        },
      })
      if (sequence === inviteQrSequence) {
        inviteQrDataUrl = dataUrl
        inviteQrError = ''
      }
    } catch {
      if (sequence === inviteQrSequence) {
        inviteQrDataUrl = ''
        inviteQrError = 'Invite QR unavailable'
      }
    }
  }

  async function onStartInviteScan() {
    if (!navigator.mediaDevices?.getUserMedia) {
      inviteScanError = 'Live QR scanning is unavailable here. Use Choose Image instead.'
      return
    }

    stopInviteScan()
    inviteScanError = ''
    inviteScanStatus = 'Requesting camera access...'
    inviteScanOpen = true
    await waitForNextPaint(window)

    if (!inviteScanVideo) {
      inviteScanOpen = false
      inviteScanStatus = ''
      inviteScanError = 'Scanner preview could not open'
      return
    }

    try {
      const cameraCandidates = buildInviteScanConstraintCandidates({
        mobile: Boolean(state.mobile),
      })
      inviteScanStream = await openInviteScanStream(
        navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices),
        cameraCandidates,
      )
      inviteScanVideo.srcObject = inviteScanStream
      await inviteScanVideo.play()
      inviteScanBusy = true
      inviteScanStatus = 'Point the camera at an invite QR code.'
      queueInviteScanFrame()
    } catch (err) {
      stopInviteScan()
      inviteScanStatus = ''
      inviteScanError = describeInviteScanError(err)
    }
  }

  function onCloseInviteScan() {
    stopInviteScan()
    inviteScanStatus = ''
  }

  function onChooseInviteQrImage() {
    stopInviteScan()
    inviteScanError = ''
    inviteScanStatus = ''
    inviteScanInput?.click()
  }

  async function onInviteScanFileSelected(event: Event) {
    const input = event.currentTarget as HTMLInputElement
    const file = input.files?.[0]
    input.value = ''
    if (!file) {
      return
    }

    inviteScanError = ''
    inviteScanStatus = 'Scanning QR image...'
    try {
      const objectUrl = URL.createObjectURL(file)
      try {
        const image = await new Promise<HTMLImageElement>((resolve, reject) => {
          const next = new Image()
          next.onload = () => resolve(next)
          next.onerror = () => reject(new Error('Could not read the selected image'))
          next.src = objectUrl
        })
        const invite = decodeInviteFromImageSource(
          image,
          image.naturalWidth || image.width,
          image.naturalHeight || image.height,
        )
        if (!invite) {
          throw new Error('No QR code was found in the selected image')
        }
        await handleScannedInvite(invite)
      } finally {
        URL.revokeObjectURL(objectUrl)
      }
    } catch (err) {
      inviteScanStatus = ''
      inviteScanError = String(err)
    }
  }

  function onInviteInput(event: Event) {
    const value = (event.currentTarget as HTMLInputElement).value
    inviteInputDraft = value
    scheduleInviteImportAttempt(value)
  }

  function onInvitePaste(event: ClipboardEvent) {
    const pasted = event.clipboardData?.getData('text/plain') ?? ''
    if (!pasted) {
      return
    }

    event.preventDefault()
    inviteInputDraft = pasted.trim()
    scheduleInviteImportAttempt(inviteInputDraft)
  }

  async function onImportInvite() {
    await importInviteCode(inviteInputDraft, {
      autoConnectOnSuccess: true,
    })
  }

  onDestroy(() => {
    clearInviteImportDebounce()
    stopInviteScan()
  })
</script>

<InviteSharePanel
  {state}
  {activeNetworkView}
  {inviteQrDataUrl}
  {inviteQrError}
  {inviteInputDraft}
  {inviteScanOpen}
  {inviteScanStatus}
  {inviteScanError}
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
  {onInviteInput}
  {onInvitePaste}
  {onImportInvite}
  {onStartInviteScan}
  {onChooseInviteQrImage}
  {onCloseInviteScan}
  {onInviteScanFileSelected}
  bind:inviteScanInput
  bind:inviteScanVideo
  {onAddParticipant}
  {lanPairingHelpText}
/>
