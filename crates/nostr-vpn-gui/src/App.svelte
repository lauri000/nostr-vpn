<script lang="ts">
  import { onDestroy, onMount } from 'svelte'

  import {
    addParticipant,
    addRelay,
    connectSession,
    disconnectSession,
    removeParticipant,
    removeRelay,
    tick,
    updateSettings,
  } from './lib/tauri'
  import type { SettingsPatch, UiState } from './lib/types'

  let state: UiState | null = null
  let participantInput = ''
  let relayInput = ''
  let error = ''

  let networkIdDraft = ''
  let nodeNameDraft = ''
  let endpointDraft = ''
  let tunnelIpDraft = ''
  let listenPortDraft = ''
  let draftsInitialized = false

  const debouncers = new Map<string, number>()
  let pollHandle: number | null = null

  const short = (value: string, head = 12, tail = 10) => {
    if (value.length <= head + tail + 3) {
      return value
    }

    return `${value.slice(0, head)}...${value.slice(-tail)}`
  }

  async function refresh() {
    try {
      state = await tick()
      error = ''
      initializeDraftsOnce()
    } catch (err) {
      error = String(err)
    }
  }

  function initializeDraftsOnce() {
    if (!state || draftsInitialized) {
      return
    }

    networkIdDraft = state.networkId
    nodeNameDraft = state.nodeName
    endpointDraft = state.endpoint
    tunnelIpDraft = state.tunnelIp
    listenPortDraft = String(state.listenPort)
    draftsInitialized = true
  }

  function debounce(key: string, fn: () => Promise<void>, delay = 450) {
    const existing = debouncers.get(key)
    if (existing) {
      window.clearTimeout(existing)
    }

    const timer = window.setTimeout(async () => {
      debouncers.delete(key)
      await fn()
    }, delay)

    debouncers.set(key, timer)
  }

  async function runAction(action: () => Promise<UiState>) {
    try {
      state = await action()
      error = ''
      initializeDraftsOnce()
    } catch (err) {
      error = String(err)
    }
  }

  async function onAddParticipant() {
    const npub = participantInput.trim()
    if (!npub) {
      return
    }

    await runAction(() => addParticipant(npub))
    participantInput = ''
  }

  async function onAddRelay() {
    const relay = relayInput.trim()
    if (!relay) {
      return
    }

    await runAction(() => addRelay(relay))
    relayInput = ''
  }

  async function onUpdateSettings(patch: SettingsPatch) {
    await runAction(() => updateSettings(patch))
  }

  async function copyPubkey() {
    if (!state) {
      return
    }

    try {
      await navigator.clipboard.writeText(state.ownNpub)
    } catch {
      error = 'Clipboard copy failed'
    }
  }

  onMount(async () => {
    await refresh()
    pollHandle = window.setInterval(refresh, 1000)
  })

  onDestroy(() => {
    if (pollHandle) {
      window.clearInterval(pollHandle)
    }
    for (const timer of debouncers.values()) {
      window.clearTimeout(timer)
    }
  })
</script>

<main class="app-shell">
  <section class="identity-card panel">
    <div class="row spread identity-row">
      <div class="pubkey-wrap">
        <div class="label">Pubkey</div>
        <div class="pubkey">{state ? short(state.ownNpub, 18, 14) : 'Loading...'}</div>
      </div>
      <button class="btn" on:click={copyPubkey} disabled={!state}>Copy</button>
    </div>

    {#if state}
      <div class="row status-row">
        <span class={`badge ${state.sessionActive ? 'ok' : 'bad'}`}>
          VPN {state.sessionActive ? 'Connected' : 'Disconnected'}
        </span>
        <span class={`badge ${state.relayConnected ? 'ok' : 'muted'}`}>
          Relays {state.relayConnected ? 'Connected' : 'Disconnected'}
        </span>
        <span class="badge muted">
          Mesh {state.connectedPeerCount}/{state.expectedPeerCount}
        </span>
      </div>
    {/if}
  </section>

  {#if error}
    <section class="panel error">{error}</section>
  {/if}

  {#if state}
    <section class="panel">
      <div class="section-title-row">
        <h2>Participants</h2>
        <div class="section-meta">Online: {state.connectedPeerCount}/{state.expectedPeerCount}</div>
      </div>

      <div class="row form-row">
        <input
          class="text-input"
          placeholder="Add participant (npub)"
          bind:value={participantInput}
          on:keydown={(event) => event.key === 'Enter' && onAddParticipant()}
        />
        <button class="btn" on:click={onAddParticipant}>Add</button>
      </div>

      <div class="stack rows">
        {#each state.participants as participant}
          <div class="item-row">
            <div class="item-main">
              <div class="item-title">{short(participant.npub, 22, 12)}</div>
              <div class="item-sub">{participant.statusText} | {participant.lastSignalText} | {participant.tunnelIp}</div>
            </div>
            <span class={`badge ${participant.state === 'online' ? 'ok' : participant.state === 'offline' ? 'bad' : participant.state === 'local' ? 'muted' : 'warn'}`}>
              {participant.state}
            </span>
            <button class="btn ghost" on:click={() => runAction(() => removeParticipant(participant.npub))}>
              Remove
            </button>
          </div>
        {/each}
      </div>

      {#if state.lanPeers.length > 0}
        <div class="lan-title">LAN discovery (auto, while no peers configured)</div>
        <div class="stack rows">
          {#each state.lanPeers as peer}
            <div class="item-row">
              <div class="item-main">
                <div class="item-title">{short(peer.npub, 22, 12)}</div>
                <div class="item-sub">{peer.nodeName} | {peer.endpoint} | seen {peer.lastSeenText}</div>
              </div>
              {#if peer.configured}
                <span class="badge ok">configured</span>
              {:else}
                <button class="btn" on:click={() => runAction(() => addParticipant(peer.npub))}>Add</button>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section class="panel">
      <div class="section-title-row">
        <h2>Relays</h2>
        <div class="section-meta relay-health">
          <span class="ok-text">{state.relaySummary.up} up</span>
          <span class="bad-text">{state.relaySummary.down} down</span>
          <span class="warn-text">{state.relaySummary.checking} checking</span>
          <span class="muted-text">{state.relaySummary.unknown} unknown</span>
        </div>
      </div>

      <div class="row form-row">
        <input
          class="text-input"
          placeholder="Add relay URL"
          bind:value={relayInput}
          on:keydown={(event) => event.key === 'Enter' && onAddRelay()}
        />
        <button class="btn" on:click={onAddRelay}>Add</button>
      </div>

      <div class="stack rows">
        {#each state.relays as relay}
          <div class="item-row">
            <div class="item-main">
              <div class="item-title relay-url">{relay.url}</div>
              <div class="item-sub">{relay.statusText}</div>
            </div>
            <span class={`badge ${relay.state === 'up' ? 'ok' : relay.state === 'down' ? 'bad' : relay.state === 'checking' ? 'warn' : 'muted'}`}>
              {relay.state}
            </span>
            <button class="btn ghost" on:click={() => runAction(() => removeRelay(relay.url))}>Remove</button>
          </div>
        {/each}
      </div>
    </section>

    <section class="panel">
      <div class="section-title-row">
        <h2>Settings</h2>
      </div>

      <div class="row spread settings-action-row">
        <div class="config-path">Config: {state.configPath}</div>
        {#if state.sessionActive}
          <button class="btn bad" on:click={() => runAction(disconnectSession)}>Disconnect</button>
        {:else}
          <button class="btn" on:click={() => runAction(connectSession)}>Connect</button>
        {/if}
      </div>

      <label class="toggle-row">
        <input
          type="checkbox"
          checked={state.autoDisconnectRelaysWhenMeshReady}
          on:change={(event) =>
            onUpdateSettings({
              autoDisconnectRelaysWhenMeshReady: (event.target as HTMLInputElement).checked,
            })}
        />
        <span>Auto-disconnect relays when mesh is ready</span>
      </label>

      <div class="field-grid">
        <label>
          <span>Fallback Network ID</span>
          <input
            class="text-input"
            bind:value={networkIdDraft}
            on:input={() => debounce('networkId', () => onUpdateSettings({ networkId: networkIdDraft }))}
          />
        </label>

        <label>
          <span>Node Name</span>
          <input
            class="text-input"
            bind:value={nodeNameDraft}
            on:input={() => debounce('nodeName', () => onUpdateSettings({ nodeName: nodeNameDraft }))}
          />
        </label>

        <label>
          <span>Endpoint</span>
          <input
            class="text-input"
            bind:value={endpointDraft}
            on:input={() => debounce('endpoint', () => onUpdateSettings({ endpoint: endpointDraft }))}
          />
        </label>

        <label>
          <span>Tunnel IP</span>
          <input
            class="text-input"
            bind:value={tunnelIpDraft}
            on:input={() => debounce('tunnelIp', () => onUpdateSettings({ tunnelIp: tunnelIpDraft }))}
          />
        </label>

        <label>
          <span>Listen Port</span>
          <input
            class="text-input"
            bind:value={listenPortDraft}
            on:input={() =>
              debounce('listenPort', async () => {
                const parsed = Number.parseInt(listenPortDraft, 10)
                if (!Number.isNaN(parsed) && parsed > 0 && parsed <= 65535) {
                  await onUpdateSettings({ listenPort: parsed })
                }
              })}
          />
        </label>
      </div>
    </section>
  {/if}
</main>
