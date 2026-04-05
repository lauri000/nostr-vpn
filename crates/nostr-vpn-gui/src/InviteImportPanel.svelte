<script lang="ts">
  import { Check, Copy } from 'lucide-svelte'

  import {
    inviteInviterParticipant,
    joinRequestButtonLabel,
    joinRequestStatusText,
  } from './lib/app-view'
  import type { NetworkView, UiState } from './lib/types'

  export let activeNetworkView: NetworkView
  export let inviteInputDraft = ''
  export let inviteScanOpen = false
  export let inviteScanStatus = ''
  export let inviteScanError = ''
  export let participantInputDrafts: Record<string, string>
  export let participantAddAliasDrafts: Record<string, string>
  export let onInviteInput: (event: Event) => void
  export let onInvitePaste: (event: ClipboardEvent) => void
  export let onImportInvite: () => Promise<void>
  export let onStartInviteScan: () => Promise<void>
  export let onChooseInviteQrImage: () => void
  export let onCloseInviteScan: () => void
  export let onInviteScanFileSelected: (event: Event) => Promise<void>
  export let onAddParticipant: (networkId: string) => Promise<void>
  export let onRequestNetworkJoin: (networkId: string) => Promise<void>
  export let inviteScanInput: HTMLInputElement | null = null
  export let inviteScanVideo: HTMLVideoElement | null = null

  $: hasInviteRequester = !!activeNetworkView.inviteInviterNpub
</script>

<div class="participant-add-panel network-onboarding-panel">
  <div class="participant-add-label">Add devices</div>
  <div class="invite-help">
    Fastest: paste an invite from another device. LAN pairing can also broadcast yours nearby for 15 minutes.
  </div>
  {#if !activeNetworkView.localIsAdmin}
    <div class="config-path">Only admins can change the participant list for this network.</div>
  {/if}
  {#if hasInviteRequester}
    <div class="mesh-share-actions">
      <button
        class="btn"
        data-testid="request-network-join"
        on:click={() => onRequestNetworkJoin(activeNetworkView.id)}
        disabled={
          Boolean(activeNetworkView.outboundJoinRequest) ||
          inviteInviterParticipant(activeNetworkView)?.state === 'online'
        }
      >
        {joinRequestButtonLabel(activeNetworkView)}
      </button>
      {#if activeNetworkView.outboundJoinRequest}
        <span class="badge warn">
          Requested {activeNetworkView.outboundJoinRequest.requestedAtText}
        </span>
      {:else if inviteInviterParticipant(activeNetworkView)?.state === 'online'}
        <span class="badge ok">Connected</span>
      {/if}
    </div>
    <div class="config-path">{joinRequestStatusText(activeNetworkView)}</div>
  {/if}
  <div class="invite-import-fields">
    <input
      class="text-input invite-import-input"
      placeholder="nvpn://invite/..."
      data-testid="invite-input"
      value={inviteInputDraft}
      on:input={onInviteInput}
      on:paste={onInvitePaste}
      on:keydown={(event) => event.key === 'Enter' && onImportInvite()}
    />
  </div>
  <div class="invite-scan-actions">
    <button class="btn" data-testid="invite-scan-start" on:click={onStartInviteScan}>
      Scan QR
    </button>
    <button class="btn" data-testid="invite-scan-image" on:click={onChooseInviteQrImage}>
      Choose Image
    </button>
    <input
      class="invite-scan-file-input"
      type="file"
      accept="image/*"
      capture="environment"
      bind:this={inviteScanInput}
      on:change={onInviteScanFileSelected}
    />
  </div>
  {#if inviteScanOpen}
    <div class="invite-scan-panel">
      <div class="invite-scan-preview">
        <video
          class="invite-scan-video"
          bind:this={inviteScanVideo}
          autoplay
          muted
          playsinline
        ></video>
        <div class="invite-scan-reticle" aria-hidden="true"></div>
      </div>
      <div class="invite-qr-caption">
        Point the camera at an invite QR. Use Cancel in the next prompt to just fill the field.
      </div>
      <button class="btn" data-testid="invite-scan-close" on:click={onCloseInviteScan}>
        Close Scanner
      </button>
    </div>
  {/if}
  {#if inviteScanStatus}
    <div class="config-path">{inviteScanStatus}</div>
  {/if}
  {#if inviteScanError}
    <div class="config-path mesh-id-note-error">{inviteScanError}</div>
  {/if}
  <div class="participant-add-separator">or add one manually</div>
  <div class="participant-add-fields">
    <input
      class="text-input participant-add-npub"
      placeholder="Participant npub"
      data-testid="participant-input"
      disabled={!activeNetworkView.localIsAdmin}
      value={participantInputDrafts[activeNetworkView.id] ?? ''}
      on:input={(event) =>
        (participantInputDrafts = {
          ...participantInputDrafts,
          [activeNetworkView.id]: (event.currentTarget as HTMLInputElement).value,
        })}
      on:keydown={(event) => event.key === 'Enter' && onAddParticipant(activeNetworkView.id)}
    />
    <input
      class="text-input participant-add-alias"
      placeholder="Alias (optional)"
      data-testid="participant-add-alias-input"
      disabled={!activeNetworkView.localIsAdmin}
      value={participantAddAliasDrafts[activeNetworkView.id] ?? ''}
      on:input={(event) =>
        (participantAddAliasDrafts = {
          ...participantAddAliasDrafts,
          [activeNetworkView.id]: (event.currentTarget as HTMLInputElement).value,
        })}
      on:keydown={(event) => event.key === 'Enter' && onAddParticipant(activeNetworkView.id)}
    />
    <button
      class="btn participant-add-btn"
      data-testid="participant-add"
      disabled={!activeNetworkView.localIsAdmin}
      on:click={() => onAddParticipant(activeNetworkView.id)}
    >
      Add
    </button>
  </div>
</div>
