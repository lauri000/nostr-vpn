<script lang="ts">
  import type { SettingsPatch, UiState } from './lib/types'

  export let state: UiState
  export let cliActionStatus = ''
  export let autostartReady = false
  export let autostartUpdating = false
  export let cliInstallSupported = false
  export let startupSettingsSupported = false
  export let trayBehaviorSupported = false
  export let magicDnsSuffixDraft = ''
  export let endpointDraft = ''
  export let tunnelIpDraft = ''
  export let listenPortDraft = ''
  export let onInstallCli: () => Promise<void>
  export let onUninstallCli: () => Promise<void>
  export let onToggleAutostart: (enabled: boolean) => Promise<void>
  export let onUpdateSettings: (patch: SettingsPatch) => Promise<void>
  export let debounce: (key: string, fn: () => Promise<void>, delay?: number) => void
</script>

<details class="panel collapsible-panel">
  <summary class="collapsible-summary">
    <div>
      <div class="panel-kicker">System</div>
      <h2>Device & App</h2>
    </div>
    <div class="section-meta">
      {cliInstallSupported || startupSettingsSupported || trayBehaviorSupported
        ? 'Node, DNS & startup'
        : 'Node & DNS'}
    </div>
  </summary>

  <div class="collapsible-body">
    <div class="row settings-action-row">
      <div class="config-path" data-testid="app-version">Version: {state.appVersion}</div>
    </div>
    <div class="row settings-action-row">
      <div class="config-path">Config: {state.configPath}</div>
    </div>
    {#if cliInstallSupported}
      <div class="row spread settings-action-row">
        <div class="config-path">Terminal CLI</div>
        <div class="row cli-actions-row">
          <button class="btn" data-testid="install-cli-btn" on:click={() => onInstallCli()}>
            {state.cliInstalled ? 'Reinstall CLI' : 'Install CLI'}
          </button>
          <button
            class="btn ghost"
            data-testid="uninstall-cli-btn"
            on:click={() => onUninstallCli()}
            disabled={!state.cliInstalled}
          >
            Uninstall CLI
          </button>
        </div>
      </div>
      {#if cliActionStatus}
        <div class="config-path">{cliActionStatus}</div>
      {/if}
    {/if}
    <div class="config-path" data-testid="magic-dns-status">DNS: {state.magicDnsStatus}</div>

    {#if startupSettingsSupported}
      <label class="toggle-row">
        <input
          type="checkbox"
          data-testid="autostart-toggle"
          checked={state.launchOnStartup}
          disabled={!autostartReady || autostartUpdating}
          on:change={(event) =>
            onToggleAutostart((event.currentTarget as HTMLInputElement).checked)}
        />
        <span>Launch on system startup</span>
      </label>
    {/if}

    {#if trayBehaviorSupported}
      <label class="toggle-row">
        <input
          type="checkbox"
          checked={state.closeToTrayOnClose}
          on:change={(event) =>
            onUpdateSettings({
              closeToTrayOnClose: (event.currentTarget as HTMLInputElement).checked,
            })}
        />
        <span>Keep running in menu bar when window is closed</span>
      </label>
    {/if}

    <div class="field-grid">
      <label>
        <span>MagicDNS Suffix (Optional)</span>
        <input
          class="text-input"
          data-testid="magic-dns-suffix-input"
          bind:value={magicDnsSuffixDraft}
          on:input={() =>
            debounce('magicDnsSuffix', () =>
              onUpdateSettings({ magicDnsSuffix: magicDnsSuffixDraft }))}
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
  </div>
</details>
