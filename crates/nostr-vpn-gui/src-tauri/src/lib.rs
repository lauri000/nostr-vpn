use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use nostr_sdk::prelude::{PublicKey, ToBech32};
use nostr_vpn_core::config::{
    AppConfig, derive_mesh_tunnel_ip, maybe_autoconfigure_node, normalize_nostr_pubkey,
};
use nostr_vpn_core::control::PeerAnnouncement;
use nostr_vpn_core::signaling::{NostrSignalingClient, SignalPayload};
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::runtime::Runtime;

const LAN_DISCOVERY_ADDR: [u8; 4] = [239, 255, 73, 73];
const LAN_DISCOVERY_PORT: u16 = 38911;
const LAN_DISCOVERY_STALE_AFTER_SECS: u64 = 16;

#[derive(Debug, Clone)]
struct RelayCheckResult {
    relay: String,
    latency_ms: u128,
    error: Option<String>,
    checked_at: SystemTime,
}

#[derive(Debug, Clone, Default)]
struct RelayStatus {
    checking: bool,
    latency_ms: Option<u128>,
    error: Option<String>,
    checked_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct RelaySummary {
    up: usize,
    down: usize,
    checking: usize,
    unknown: usize,
}

#[derive(Debug, Clone)]
struct PeerCheckResult {
    participant: String,
    reachable: bool,
    latency_ms: Option<u128>,
    error: Option<String>,
    checked_at: SystemTime,
}

#[derive(Debug, Clone, Default)]
struct PeerLinkStatus {
    checking: bool,
    reachable: Option<bool>,
    latency_ms: Option<u128>,
    error: Option<String>,
    checked_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfiguredPeerStatus {
    Local,
    Checking,
    Online,
    Offline,
    Unknown,
}

#[derive(Debug, Clone)]
struct LanPeerRecord {
    npub: String,
    node_name: String,
    endpoint: String,
    last_seen: SystemTime,
}

#[derive(Debug, Clone)]
struct LanDiscoverySignal {
    npub: String,
    node_name: String,
    endpoint: String,
    seen_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LanAnnouncement {
    v: u8,
    npub: String,
    node_name: String,
    endpoint: String,
    timestamp: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayView {
    url: String,
    state: String,
    status_text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ParticipantView {
    npub: String,
    pubkey_hex: String,
    tunnel_ip: String,
    state: String,
    status_text: String,
    last_signal_text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LanPeerView {
    npub: String,
    node_name: String,
    endpoint: String,
    last_seen_text: String,
    configured: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiState {
    session_active: bool,
    relay_connected: bool,
    session_status: String,
    config_path: String,
    own_npub: String,
    own_pubkey_hex: String,
    node_id: String,
    node_name: String,
    endpoint: String,
    tunnel_ip: String,
    listen_port: u16,
    network_id: String,
    effective_network_id: String,
    auto_disconnect_relays_when_mesh_ready: bool,
    connected_peer_count: usize,
    expected_peer_count: usize,
    mesh_ready: bool,
    participants: Vec<ParticipantView>,
    relays: Vec<RelayView>,
    relay_summary: RelaySummary,
    lan_peers: Vec<LanPeerView>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatch {
    node_name: Option<String>,
    endpoint: Option<String>,
    tunnel_ip: Option<String>,
    listen_port: Option<u16>,
    network_id: Option<String>,
    auto_disconnect_relays_when_mesh_ready: Option<bool>,
}

struct NvpnBackend {
    runtime: Runtime,
    config_path: PathBuf,
    config: AppConfig,

    session_status: String,
    session_active: bool,
    relay_connected: bool,
    client: Option<Arc<NostrSignalingClient>>,
    signal_rx: Option<mpsc::Receiver<nostr_vpn_core::signaling::SignalEnvelope>>,

    relay_status: HashMap<String, RelayStatus>,
    relay_check_rx: Option<mpsc::Receiver<Vec<RelayCheckResult>>>,
    relay_check_inflight: bool,
    next_relay_check_at: Option<Instant>,
    next_relay_retry_at: Option<Instant>,

    peer_status: HashMap<String, PeerLinkStatus>,
    peer_signal_seen_at: HashMap<String, SystemTime>,
    peer_check_rx: Option<mpsc::Receiver<Vec<PeerCheckResult>>>,
    peer_check_inflight: bool,
    next_peer_check_at: Option<Instant>,

    autosave_pending: bool,
    autosave_due_at: Option<Instant>,

    lan_discovery_running: bool,
    lan_discovery_rx: Option<mpsc::Receiver<LanDiscoverySignal>>,
    lan_discovery_stop: Option<Arc<AtomicBool>>,
    lan_peers: HashMap<String, LanPeerRecord>,
}

impl NvpnBackend {
    fn new() -> Result<Self> {
        let runtime = Runtime::new().context("failed to create tokio runtime")?;
        let config_path = default_config_path();

        let mut config = if config_path.exists() {
            AppConfig::load(&config_path).context("failed to load config")?
        } else {
            let generated = AppConfig::generated();
            let _ = generated.save(&config_path);
            generated
        };

        config.ensure_defaults();
        maybe_autoconfigure_node(&mut config);

        let relay_status = config
            .nostr
            .relays
            .iter()
            .map(|relay| (relay.clone(), RelayStatus::default()))
            .collect::<HashMap<_, _>>();

        let peer_status = config
            .participants
            .iter()
            .map(|participant| (participant.clone(), PeerLinkStatus::default()))
            .collect::<HashMap<_, _>>();

        let mut backend = Self {
            runtime,
            config_path,
            config,
            session_status: "Disconnected".to_string(),
            session_active: false,
            relay_connected: false,
            client: None,
            signal_rx: None,
            relay_status,
            relay_check_rx: None,
            relay_check_inflight: false,
            next_relay_check_at: None,
            next_relay_retry_at: None,
            peer_status,
            peer_signal_seen_at: HashMap::new(),
            peer_check_rx: None,
            peer_check_inflight: false,
            next_peer_check_at: None,
            autosave_pending: false,
            autosave_due_at: None,
            lan_discovery_running: false,
            lan_discovery_rx: None,
            lan_discovery_stop: None,
            lan_peers: HashMap::new(),
        };

        backend.ensure_relay_status_entries();
        backend.ensure_peer_status_entries();
        backend.maybe_refresh_lan_discovery();

        Ok(backend)
    }

    fn connect_session(&mut self) -> Result<()> {
        if self.session_active {
            return Ok(());
        }

        self.session_active = true;
        if let Err(error) = self.connect_relays() {
            self.session_active = false;
            self.session_status = format!("Connect failed: {error}");
            return Err(error);
        }

        self.session_status = "Connected".to_string();
        Ok(())
    }

    fn connect_relays(&mut self) -> Result<()> {
        if self.relay_connected {
            return Ok(());
        }

        if self.config.nostr.relays.is_empty() {
            return Err(anyhow!("at least one relay is required"));
        }

        maybe_autoconfigure_node(&mut self.config);

        let relays = self.config.nostr.relays.clone();
        let network_id = self.config.effective_network_id();
        let client = Arc::new(NostrSignalingClient::from_secret_key(
            network_id,
            &self.config.nostr.secret_key,
            self.config.participant_pubkeys_hex(),
        )?);

        self.runtime.block_on(client.connect(&relays))?;

        let (tx, rx) = mpsc::channel();
        let recv_client = client.clone();
        self.runtime.spawn(async move {
            loop {
                let Some(message) = recv_client.recv().await else {
                    break;
                };

                if tx.send(message).is_err() {
                    break;
                }
            }
        });

        self.client = Some(client);
        self.signal_rx = Some(rx);
        self.relay_connected = true;
        self.next_relay_retry_at = None;

        self.ensure_peer_status_entries();
        self.start_relay_check(4);
        self.next_relay_check_at = Some(Instant::now() + Duration::from_secs(45));
        self.start_peer_check(2);
        self.next_peer_check_at = Some(Instant::now() + Duration::from_secs(12));

        if let Err(error) = self.publish_announcement()
            && !is_no_participants_error(&error)
        {
            self.session_status = format!("Connected, announce failed: {error}");
        }

        Ok(())
    }

    fn disconnect_session(&mut self) {
        self.session_active = false;
        self.disconnect_relays();

        self.relay_check_inflight = false;
        self.relay_check_rx = None;
        self.next_relay_check_at = None;
        self.next_relay_retry_at = None;

        self.peer_check_inflight = false;
        self.peer_check_rx = None;
        self.next_peer_check_at = None;

        self.session_status = "Disconnected".to_string();
    }

    fn disconnect_relays(&mut self) {
        if let Some(client) = self.client.take() {
            self.runtime.block_on(client.disconnect());
        }

        self.signal_rx = None;
        self.relay_connected = false;
    }

    fn publish_announcement(&self) -> Result<()> {
        let Some(client) = self.client.clone() else {
            return Err(anyhow!("connect first"));
        };

        let announcement = PeerAnnouncement {
            node_id: self.config.node.id.clone(),
            public_key: self.config.node.public_key.clone(),
            endpoint: self.config.node.endpoint.clone(),
            tunnel_ip: self.config.node.tunnel_ip.clone(),
            timestamp: unix_timestamp(),
        };

        self.runtime
            .block_on(client.publish(SignalPayload::Announce(announcement)))
    }

    fn start_relay_check(&mut self, timeout_secs: u64) {
        self.ensure_relay_status_entries();

        if self.relay_check_inflight || self.config.nostr.relays.is_empty() || !self.relay_connected
        {
            return;
        }

        for relay in &self.config.nostr.relays {
            self.relay_status
                .entry(relay.clone())
                .and_modify(|status| {
                    status.checking = true;
                    status.error = None;
                })
                .or_insert_with(|| RelayStatus {
                    checking: true,
                    ..RelayStatus::default()
                });
        }

        let relays = self.config.nostr.relays.clone();
        let network_id = self.config.effective_network_id();
        let secret_key = self.config.nostr.secret_key.clone();
        let participants = self.config.participant_pubkeys_hex();

        let (tx, rx) = mpsc::channel();
        self.relay_check_rx = Some(rx);
        self.relay_check_inflight = true;

        self.runtime.spawn(async move {
            let mut checks = Vec::with_capacity(relays.len());

            for relay in relays {
                let started = Instant::now();
                let probe = tokio::time::timeout(Duration::from_secs(timeout_secs.max(1)), async {
                    let client = NostrSignalingClient::from_secret_key(
                        network_id.clone(),
                        &secret_key,
                        participants.clone(),
                    )?;
                    client.connect(std::slice::from_ref(&relay)).await?;
                    client.disconnect().await;
                    Result::<(), anyhow::Error>::Ok(())
                })
                .await;

                let error = match probe {
                    Ok(Ok(())) => None,
                    Ok(Err(err)) => Some(err.to_string()),
                    Err(_) => Some("timeout".to_string()),
                };

                checks.push(RelayCheckResult {
                    relay,
                    latency_ms: started.elapsed().as_millis(),
                    error,
                    checked_at: SystemTime::now(),
                });
            }

            let _ = tx.send(checks);
        });
    }

    fn handle_relay_checks(&mut self) {
        let recv_result = self
            .relay_check_rx
            .as_ref()
            .map(|receiver| receiver.try_recv());

        match recv_result {
            Some(Ok(results)) => {
                for result in results {
                    self.relay_status.insert(
                        result.relay,
                        RelayStatus {
                            checking: false,
                            latency_ms: Some(result.latency_ms),
                            error: result.error,
                            checked_at: Some(result.checked_at),
                        },
                    );
                }
                self.relay_check_inflight = false;
                self.relay_check_rx = None;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.relay_check_inflight = false;
                self.relay_check_rx = None;
            }
            _ => {}
        }
    }

    fn maybe_schedule_periodic_relay_check(&mut self) {
        if !self.session_active || !self.relay_connected || self.relay_check_inflight {
            return;
        }

        let now = Instant::now();
        let due = self
            .next_relay_check_at
            .is_none_or(|next_check| now >= next_check);

        if due {
            self.start_relay_check(4);
            self.next_relay_check_at = Some(now + Duration::from_secs(45));
        }
    }

    fn start_peer_check(&mut self, timeout_secs: u64) {
        self.ensure_peer_status_entries();

        if self.peer_check_inflight || self.config.participants.is_empty() || !self.session_active {
            return;
        }

        let own_pubkey = self.config.own_nostr_pubkey_hex().ok();
        let participants = self.config.participants.clone();

        for participant in &participants {
            if Some(participant.as_str()) == own_pubkey.as_deref() {
                continue;
            }

            self.peer_status
                .entry(participant.clone())
                .and_modify(|status| {
                    status.checking = true;
                    status.error = None;
                })
                .or_insert_with(|| PeerLinkStatus {
                    checking: true,
                    ..PeerLinkStatus::default()
                });
        }

        let (tx, rx) = mpsc::channel();
        self.peer_check_rx = Some(rx);
        self.peer_check_inflight = true;

        self.runtime.spawn(async move {
            let mut results = Vec::new();

            for participant in &participants {
                if Some(participant.as_str()) == own_pubkey.as_deref() {
                    continue;
                }

                let Some(tunnel_ip) = derive_mesh_tunnel_ip(&participants, participant) else {
                    results.push(PeerCheckResult {
                        participant: participant.clone(),
                        reachable: false,
                        latency_ms: None,
                        error: Some("failed to derive tunnel ip".to_string()),
                        checked_at: SystemTime::now(),
                    });
                    continue;
                };

                let target_ip = tunnel_ip
                    .split('/')
                    .next()
                    .unwrap_or(&tunnel_ip)
                    .to_string();

                let probe =
                    tokio::task::spawn_blocking(move || run_ping_probe(&target_ip, timeout_secs))
                        .await;

                let (reachable, latency_ms, error) = match probe {
                    Ok(result) => result,
                    Err(join_error) => (
                        false,
                        None,
                        Some(format!("probe task failed: {join_error}")),
                    ),
                };

                results.push(PeerCheckResult {
                    participant: participant.clone(),
                    reachable,
                    latency_ms,
                    error,
                    checked_at: SystemTime::now(),
                });
            }

            let _ = tx.send(results);
        });
    }

    fn handle_peer_checks(&mut self) {
        let recv_result = self
            .peer_check_rx
            .as_ref()
            .map(|receiver| receiver.try_recv());

        match recv_result {
            Some(Ok(results)) => {
                for result in results {
                    self.peer_status.insert(
                        result.participant,
                        PeerLinkStatus {
                            checking: false,
                            reachable: Some(result.reachable),
                            latency_ms: result.latency_ms,
                            error: result.error,
                            checked_at: Some(result.checked_at),
                        },
                    );
                }
                self.peer_check_inflight = false;
                self.peer_check_rx = None;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.peer_check_inflight = false;
                self.peer_check_rx = None;
            }
            _ => {}
        }
    }

    fn maybe_schedule_periodic_peer_check(&mut self) {
        if !self.session_active || self.peer_check_inflight {
            return;
        }

        let now = Instant::now();
        let due = self
            .next_peer_check_at
            .is_none_or(|next_check| now >= next_check);

        if due {
            self.start_peer_check(2);
            self.next_peer_check_at = Some(now + Duration::from_secs(12));
        }
    }

    fn handle_signals(&mut self) {
        if let Some(rx) = &self.signal_rx {
            while let Ok(message) = rx.try_recv() {
                self.peer_signal_seen_at
                    .insert(message.sender_pubkey.clone(), SystemTime::now());

                match message.payload {
                    SignalPayload::Announce(_) => {
                        let state = self.peer_status.entry(message.sender_pubkey).or_default();
                        state.error = None;
                    }
                    SignalPayload::Disconnect { .. } => {
                        self.peer_status.insert(
                            message.sender_pubkey,
                            PeerLinkStatus {
                                checking: false,
                                reachable: Some(false),
                                latency_ms: None,
                                error: Some("peer disconnected".to_string()),
                                checked_at: Some(SystemTime::now()),
                            },
                        );
                    }
                }
            }
        }
    }

    fn maybe_auto_relay_policy(&mut self) {
        if !self.session_active {
            return;
        }

        let expected = expected_peer_count(&self.config);
        let connected = connected_configured_peer_count(&self.config, &self.peer_status);

        if self.config.auto_disconnect_relays_when_mesh_ready
            && is_mesh_complete(connected, expected)
        {
            if self.relay_connected {
                self.disconnect_relays();
                self.session_status =
                    format!("Mesh ready ({connected}/{expected}) - relay connections paused");
            }
            return;
        }

        if !self.relay_connected {
            let now = Instant::now();
            let due = self
                .next_relay_retry_at
                .is_none_or(|retry_at| now >= retry_at);
            if !due {
                return;
            }

            if let Err(error) = self.connect_relays() {
                self.session_status = format!("Relay reconnect failed: {error}");
                self.next_relay_retry_at = Some(now + Duration::from_secs(5));
            }
        }
    }

    fn add_participant(&mut self, npub: &str) -> Result<()> {
        let input = npub.trim();
        if input.is_empty() {
            return Err(anyhow!("participant npub is empty"));
        }
        if !input.starts_with("npub1") {
            return Err(anyhow!("participant must be an npub"));
        }

        let normalized = normalize_nostr_pubkey(input)?;
        if self
            .config
            .participants
            .iter()
            .any(|participant| participant == &normalized)
        {
            return Ok(());
        }

        self.config.participants.push(normalized.clone());
        self.config.participants.sort();
        self.config.participants.dedup();
        self.peer_status.entry(normalized).or_default();
        maybe_autoconfigure_node(&mut self.config);

        self.schedule_autosave();
        self.ensure_peer_status_entries();
        self.restart_relay_if_needed()?;
        self.maybe_refresh_lan_discovery();

        Ok(())
    }

    fn remove_participant(&mut self, npub_or_hex: &str) -> Result<()> {
        let normalized = normalize_nostr_pubkey(npub_or_hex)?;
        let previous_len = self.config.participants.len();
        self.config
            .participants
            .retain(|participant| participant != &normalized);

        if self.config.participants.len() == previous_len {
            return Ok(());
        }

        self.peer_status.remove(&normalized);
        self.peer_signal_seen_at.remove(&normalized);

        maybe_autoconfigure_node(&mut self.config);
        self.schedule_autosave();
        self.ensure_peer_status_entries();
        self.restart_relay_if_needed()?;
        self.maybe_refresh_lan_discovery();

        Ok(())
    }

    fn add_relay(&mut self, relay: &str) -> Result<()> {
        let relay = relay.trim();
        if relay.is_empty() {
            return Err(anyhow!("relay URL is empty"));
        }

        if !(relay.starts_with("ws://") || relay.starts_with("wss://")) {
            return Err(anyhow!("relay URL must start with ws:// or wss://"));
        }

        if self
            .config
            .nostr
            .relays
            .iter()
            .any(|existing| existing == relay)
        {
            return Ok(());
        }

        self.config.nostr.relays.push(relay.to_string());
        self.relay_status.entry(relay.to_string()).or_default();
        self.schedule_autosave();
        self.ensure_relay_status_entries();
        self.restart_relay_if_needed()?;

        Ok(())
    }

    fn remove_relay(&mut self, relay: &str) -> Result<()> {
        if self.config.nostr.relays.len() <= 1 {
            return Err(anyhow!("at least one relay is required"));
        }

        let previous_len = self.config.nostr.relays.len();
        self.config.nostr.relays.retain(|value| value != relay);

        if self.config.nostr.relays.len() == previous_len {
            return Ok(());
        }

        self.relay_status.remove(relay);
        self.schedule_autosave();
        self.ensure_relay_status_entries();
        self.restart_relay_if_needed()?;

        Ok(())
    }

    fn update_settings(&mut self, patch: SettingsPatch) -> Result<()> {
        let mut reconnect_required = false;

        if let Some(node_name) = patch.node_name {
            self.config.node_name = node_name;
        }

        if let Some(endpoint) = patch.endpoint {
            self.config.node.endpoint = endpoint;
        }

        if let Some(tunnel_ip) = patch.tunnel_ip {
            self.config.node.tunnel_ip = tunnel_ip;
        }

        if let Some(listen_port) = patch.listen_port {
            if listen_port == 0 {
                return Err(anyhow!("listen port must be > 0"));
            }
            self.config.node.listen_port = listen_port;
        }

        if let Some(network_id) = patch.network_id {
            self.config.network_id = network_id;
            reconnect_required = true;
        }

        if let Some(auto_disconnect_relays_when_mesh_ready) =
            patch.auto_disconnect_relays_when_mesh_ready
        {
            self.config.auto_disconnect_relays_when_mesh_ready =
                auto_disconnect_relays_when_mesh_ready;
        }

        self.config.ensure_defaults();
        maybe_autoconfigure_node(&mut self.config);

        self.schedule_autosave();

        if reconnect_required {
            self.restart_relay_if_needed()?;
        } else if self.relay_connected {
            let _ = self.publish_announcement();
        }

        Ok(())
    }

    fn restart_relay_if_needed(&mut self) -> Result<()> {
        if !self.session_active {
            return Ok(());
        }

        let was_connected = self.relay_connected;
        if self.relay_connected {
            self.disconnect_relays();
        }

        if was_connected {
            self.connect_relays()?;
        } else {
            self.next_relay_retry_at = Some(Instant::now());
        }

        Ok(())
    }

    fn persist_config(&mut self) -> Result<()> {
        if self.config.nostr.relays.is_empty() {
            return Err(anyhow!("at least one relay is required"));
        }

        self.config.ensure_defaults();
        maybe_autoconfigure_node(&mut self.config);
        self.config.save(&self.config_path)?;
        self.ensure_relay_status_entries();
        self.ensure_peer_status_entries();

        Ok(())
    }

    fn schedule_autosave(&mut self) {
        self.autosave_pending = true;
        self.autosave_due_at = Some(Instant::now() + Duration::from_millis(700));
    }

    fn maybe_run_autosave(&mut self) {
        if !self.autosave_pending {
            return;
        }

        let due = self
            .autosave_due_at
            .is_some_and(|deadline| Instant::now() >= deadline);
        if !due {
            return;
        }

        match self.persist_config() {
            Ok(()) => {
                self.autosave_pending = false;
                self.autosave_due_at = None;
            }
            Err(error) => {
                self.session_status = format!("Autosave failed: {error}");
                self.autosave_due_at = Some(Instant::now() + Duration::from_secs(2));
            }
        }
    }

    fn ensure_relay_status_entries(&mut self) {
        let configured: HashSet<String> = self.config.nostr.relays.iter().cloned().collect();
        self.relay_status
            .retain(|relay, _| configured.contains(relay));

        for relay in &self.config.nostr.relays {
            self.relay_status.entry(relay.clone()).or_default();
        }
    }

    fn ensure_peer_status_entries(&mut self) {
        let configured: HashSet<String> = self.config.participants.iter().cloned().collect();
        self.peer_status
            .retain(|participant, _| configured.contains(participant));
        self.peer_signal_seen_at
            .retain(|participant, _| configured.contains(participant));

        for participant in &self.config.participants {
            self.peer_status.entry(participant.clone()).or_default();
        }
    }

    fn relay_summary(&self) -> RelaySummary {
        let mut summary = RelaySummary::default();

        for relay in &self.config.nostr.relays {
            match self.relay_status.get(relay) {
                Some(status) if status.checking => summary.checking += 1,
                Some(status) if status.error.is_none() && status.latency_ms.is_some() => {
                    summary.up += 1;
                }
                Some(status) if status.error.is_some() => summary.down += 1,
                _ => summary.unknown += 1,
            }
        }

        summary
    }

    fn relay_state(&self, relay: &str) -> &'static str {
        match self.relay_status.get(relay) {
            Some(status) if status.checking => "checking",
            Some(status) if status.error.is_none() && status.latency_ms.is_some() => "up",
            Some(status) if status.error.is_some() => "down",
            _ => "unknown",
        }
    }

    fn relay_status_line(&self, relay: &str) -> String {
        let Some(status) = self.relay_status.get(relay) else {
            return "not checked".to_string();
        };

        if status.checking {
            return "checking...".to_string();
        }

        if let Some(error) = &status.error {
            return format!("down ({error})");
        }

        if let Some(latency_ms) = status.latency_ms {
            if let Some(checked_at) = status.checked_at {
                let age_secs = checked_at
                    .elapsed()
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or(0);
                return format!("up ({latency_ms} ms, {age_secs}s ago)");
            }
            return format!("up ({latency_ms} ms)");
        }

        "not checked".to_string()
    }

    fn configured_peer_rows(&self) -> Vec<ParticipantView> {
        let own_pubkey_hex = self.config.own_nostr_pubkey_hex().ok();
        let mut participants = self.config.participants.clone();
        participants.sort();
        participants.dedup();

        participants
            .into_iter()
            .map(|participant| {
                let tunnel_ip = derive_mesh_tunnel_ip(&self.config.participants, &participant)
                    .unwrap_or_else(|| "-".to_string());
                let state = self.peer_state_for(&participant, own_pubkey_hex.as_deref());
                let status_text = self.peer_status_line(&participant, state);
                let last_signal_text =
                    self.peer_presence_line(&participant, own_pubkey_hex.as_deref());

                ParticipantView {
                    npub: to_npub(&participant),
                    pubkey_hex: participant,
                    tunnel_ip,
                    state: peer_state_label(state).to_string(),
                    status_text,
                    last_signal_text,
                }
            })
            .collect()
    }

    fn peer_presence_line(&self, participant: &str, own_pubkey_hex: Option<&str>) -> String {
        if Some(participant) == own_pubkey_hex {
            return "self".to_string();
        }

        let Some(seen_at) = self.peer_signal_seen_at.get(participant) else {
            return "no signal yet".to_string();
        };

        let age_secs = seen_at
            .elapsed()
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        format!("signal {age_secs}s ago")
    }

    fn peer_state_for(
        &self,
        participant: &str,
        own_pubkey_hex: Option<&str>,
    ) -> ConfiguredPeerStatus {
        if Some(participant) == own_pubkey_hex {
            return ConfiguredPeerStatus::Local;
        }

        match self.peer_status.get(participant) {
            Some(status) if status.checking => ConfiguredPeerStatus::Checking,
            Some(status) if status.reachable == Some(true) => ConfiguredPeerStatus::Online,
            Some(status) if status.reachable == Some(false) => ConfiguredPeerStatus::Offline,
            _ => ConfiguredPeerStatus::Unknown,
        }
    }

    fn peer_status_line(&self, participant: &str, status: ConfiguredPeerStatus) -> String {
        match status {
            ConfiguredPeerStatus::Local => "local".to_string(),
            ConfiguredPeerStatus::Checking => "checking...".to_string(),
            ConfiguredPeerStatus::Online => {
                let Some(link) = self.peer_status.get(participant) else {
                    return "online".to_string();
                };
                let age = link.checked_at.and_then(|checked_at| {
                    checked_at.elapsed().ok().map(|elapsed| elapsed.as_secs())
                });
                match (link.latency_ms, age) {
                    (Some(latency), Some(age_secs)) => {
                        format!("online ({latency} ms, {age_secs}s ago)")
                    }
                    (Some(latency), None) => format!("online ({latency} ms)"),
                    (None, Some(age_secs)) => format!("online ({age_secs}s ago)"),
                    (None, None) => "online".to_string(),
                }
            }
            ConfiguredPeerStatus::Offline => {
                let Some(link) = self.peer_status.get(participant) else {
                    return "offline".to_string();
                };
                let age = link.checked_at.and_then(|checked_at| {
                    checked_at.elapsed().ok().map(|elapsed| elapsed.as_secs())
                });
                if let Some(error) = &link.error {
                    match age {
                        Some(age_secs) => {
                            format!(
                                "offline ({}, {age_secs}s ago)",
                                shorten_middle(error, 18, 8)
                            )
                        }
                        None => format!("offline ({})", shorten_middle(error, 18, 8)),
                    }
                } else {
                    match age {
                        Some(age_secs) => format!("offline ({age_secs}s ago)"),
                        None => "offline".to_string(),
                    }
                }
            }
            ConfiguredPeerStatus::Unknown => "not checked".to_string(),
        }
    }

    fn tick(&mut self) {
        self.handle_relay_checks();
        self.handle_peer_checks();
        self.handle_signals();

        self.maybe_schedule_periodic_relay_check();
        self.maybe_schedule_periodic_peer_check();
        self.maybe_auto_relay_policy();

        self.maybe_refresh_lan_discovery();
        self.handle_lan_discovery_events();
        self.prune_lan_peers();

        self.maybe_run_autosave();
    }

    fn maybe_refresh_lan_discovery(&mut self) {
        let should_run = self.config.participants.is_empty();

        if should_run && !self.lan_discovery_running {
            self.start_lan_discovery();
        } else if !should_run && self.lan_discovery_running {
            self.stop_lan_discovery();
            self.lan_peers.clear();
        }
    }

    fn start_lan_discovery(&mut self) {
        let own_npub = self
            .config
            .own_nostr_pubkey_hex()
            .map(|hex| to_npub(&hex))
            .unwrap_or_else(|_| self.config.nostr.public_key.clone());
        let node_name = self.config.node_name.clone();
        let endpoint = self.config.node.endpoint.clone();

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();

        self.runtime.spawn(async move {
            run_lan_discovery_loop(tx, stop_flag, own_npub, node_name, endpoint).await;
        });

        self.lan_discovery_rx = Some(rx);
        self.lan_discovery_stop = Some(stop);
        self.lan_discovery_running = true;
    }

    fn stop_lan_discovery(&mut self) {
        if let Some(stop) = self.lan_discovery_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.lan_discovery_rx = None;
        self.lan_discovery_running = false;
    }

    fn handle_lan_discovery_events(&mut self) {
        let own_npub = self
            .config
            .own_nostr_pubkey_hex()
            .map(|hex| to_npub(&hex))
            .unwrap_or_default();

        let recv_result = self
            .lan_discovery_rx
            .as_ref()
            .map(|receiver| receiver.try_recv());

        match recv_result {
            Some(Ok(event)) => {
                if event.npub == own_npub {
                    return;
                }

                self.lan_peers.insert(
                    event.npub.clone(),
                    LanPeerRecord {
                        npub: event.npub,
                        node_name: event.node_name,
                        endpoint: event.endpoint,
                        last_seen: event.seen_at,
                    },
                );

                if let Some(receiver) = &self.lan_discovery_rx {
                    while let Ok(event) = receiver.try_recv() {
                        if event.npub == own_npub {
                            continue;
                        }

                        self.lan_peers.insert(
                            event.npub.clone(),
                            LanPeerRecord {
                                npub: event.npub,
                                node_name: event.node_name,
                                endpoint: event.endpoint,
                                last_seen: event.seen_at,
                            },
                        );
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.lan_discovery_running = false;
                self.lan_discovery_rx = None;
                self.lan_discovery_stop = None;
            }
            _ => {}
        }
    }

    fn prune_lan_peers(&mut self) {
        self.lan_peers.retain(|_, peer| {
            peer.last_seen
                .elapsed()
                .map(|elapsed| elapsed.as_secs() <= LAN_DISCOVERY_STALE_AFTER_SECS)
                .unwrap_or(false)
        });
    }

    fn lan_peer_rows(&self) -> Vec<LanPeerView> {
        let mut peers = self.lan_peers.values().cloned().collect::<Vec<_>>();
        peers.sort_by(|left, right| left.npub.cmp(&right.npub));

        peers
            .into_iter()
            .map(|peer| {
                let configured = self
                    .config
                    .participants
                    .iter()
                    .filter_map(|value| self.npub_or_none(value))
                    .any(|npub| npub == peer.npub);

                let last_seen_secs = peer
                    .last_seen
                    .elapsed()
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or(0);

                LanPeerView {
                    npub: peer.npub,
                    node_name: peer.node_name,
                    endpoint: peer.endpoint,
                    last_seen_text: format!("{last_seen_secs}s ago"),
                    configured,
                }
            })
            .collect()
    }

    fn npub_or_none(&self, value: &str) -> Option<String> {
        PublicKey::from_hex(value)
            .ok()
            .and_then(|key| key.to_bech32().ok())
    }

    fn ui_state(&self) -> UiState {
        let own_pubkey_hex = self.config.own_nostr_pubkey_hex().unwrap_or_default();
        let own_npub = to_npub(&own_pubkey_hex);

        let participants = self.configured_peer_rows();
        let relays = self
            .config
            .nostr
            .relays
            .iter()
            .map(|relay| RelayView {
                url: relay.clone(),
                state: self.relay_state(relay).to_string(),
                status_text: self.relay_status_line(relay),
            })
            .collect::<Vec<_>>();

        let relay_summary = self.relay_summary();
        let expected_peer_count = expected_peer_count(&self.config);
        let connected_peer_count = connected_configured_peer_count(&self.config, &self.peer_status);

        UiState {
            session_active: self.session_active,
            relay_connected: self.relay_connected,
            session_status: self.session_status.clone(),
            config_path: self.config_path.display().to_string(),
            own_npub,
            own_pubkey_hex,
            node_id: self.config.node.id.clone(),
            node_name: self.config.node_name.clone(),
            endpoint: self.config.node.endpoint.clone(),
            tunnel_ip: self.config.node.tunnel_ip.clone(),
            listen_port: self.config.node.listen_port,
            network_id: self.config.network_id.clone(),
            effective_network_id: self.config.effective_network_id(),
            auto_disconnect_relays_when_mesh_ready: self
                .config
                .auto_disconnect_relays_when_mesh_ready,
            connected_peer_count,
            expected_peer_count,
            mesh_ready: is_mesh_complete(connected_peer_count, expected_peer_count),
            participants,
            relays,
            relay_summary,
            lan_peers: self.lan_peer_rows(),
        }
    }
}

impl Drop for NvpnBackend {
    fn drop(&mut self) {
        self.stop_lan_discovery();
        self.disconnect_relays();
    }
}

fn run_ping_probe(target: &str, timeout_secs: u64) -> (bool, Option<u128>, Option<String>) {
    let mut command = ProcessCommand::new("ping");

    #[cfg(target_os = "windows")]
    {
        command
            .arg("-n")
            .arg("1")
            .arg("-w")
            .arg((timeout_secs.max(1) * 1000).to_string())
            .arg(target);
    }

    #[cfg(target_os = "macos")]
    {
        command
            .arg("-c")
            .arg("1")
            .arg("-W")
            .arg((timeout_secs.max(1) * 1000).to_string())
            .arg(target);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        command
            .arg("-c")
            .arg("1")
            .arg("-W")
            .arg(timeout_secs.max(1).to_string())
            .arg(target);
    }

    match command.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                (true, parse_ping_latency_ms(&stdout), None)
            } else {
                let err = if stderr.trim().is_empty() {
                    stdout.trim().to_string()
                } else {
                    stderr.trim().to_string()
                };
                (false, None, Some(err))
            }
        }
        Err(error) => (false, None, Some(error.to_string())),
    }
}

fn parse_ping_latency_ms(output: &str) -> Option<u128> {
    let needle = "time=";
    let start = output.find(needle)? + needle.len();
    let raw = output[start..].split_whitespace().next()?.trim();

    if raw.starts_with('<') {
        return Some(1);
    }

    let cleaned = raw.trim_end_matches("ms").trim_end_matches("msec");
    let parsed = cleaned.parse::<f64>().ok()?;
    Some(parsed.round() as u128)
}

fn shorten_middle(value: &str, head: usize, tail: usize) -> String {
    if value.len() <= head + tail + 3 {
        return value.to_string();
    }

    format!(
        "{}...{}",
        value.chars().take(head).collect::<String>(),
        value
            .chars()
            .rev()
            .take(tail)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    )
}

fn expected_peer_count(config: &AppConfig) -> usize {
    if config.participants.is_empty() {
        return 0;
    }

    let mut expected = config.participants.len();
    if let Ok(own_pubkey) = config.own_nostr_pubkey_hex()
        && config
            .participants
            .iter()
            .any(|participant| participant == &own_pubkey)
    {
        expected = expected.saturating_sub(1);
    }

    expected
}

fn connected_configured_peer_count(
    config: &AppConfig,
    peer_status: &HashMap<String, PeerLinkStatus>,
) -> usize {
    let own_pubkey = config.own_nostr_pubkey_hex().ok();

    config
        .participants
        .iter()
        .filter(|participant| Some(participant.as_str()) != own_pubkey.as_deref())
        .filter(|participant| {
            peer_status
                .get(*participant)
                .and_then(|status| status.reachable)
                .unwrap_or(false)
        })
        .count()
}

fn is_mesh_complete(connected: usize, expected: usize) -> bool {
    expected > 0 && connected >= expected
}

fn peer_state_label(state: ConfiguredPeerStatus) -> &'static str {
    match state {
        ConfiguredPeerStatus::Local => "local",
        ConfiguredPeerStatus::Checking => "checking",
        ConfiguredPeerStatus::Online => "online",
        ConfiguredPeerStatus::Offline => "offline",
        ConfiguredPeerStatus::Unknown => "unknown",
    }
}

fn default_config_path() -> PathBuf {
    if let Some(mut path) = dirs::config_dir() {
        path.push("nvpn");
        path.push("config.toml");
        return path;
    }

    PathBuf::from("nvpn.toml")
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn to_npub(pubkey_hex: &str) -> String {
    PublicKey::from_hex(pubkey_hex)
        .ok()
        .and_then(|pubkey| pubkey.to_bech32().ok())
        .unwrap_or_else(|| pubkey_hex.to_string())
}

fn is_no_participants_error(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .contains("no configured participants to send private signaling message to")
}

async fn run_lan_discovery_loop(
    tx: mpsc::Sender<LanDiscoverySignal>,
    stop_flag: Arc<AtomicBool>,
    own_npub: String,
    node_name: String,
    endpoint: String,
) {
    let multicast = Ipv4Addr::new(
        LAN_DISCOVERY_ADDR[0],
        LAN_DISCOVERY_ADDR[1],
        LAN_DISCOVERY_ADDR[2],
        LAN_DISCOVERY_ADDR[3],
    );
    let target = SocketAddr::from((LAN_DISCOVERY_ADDR, LAN_DISCOVERY_PORT));

    let std_socket = match std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, LAN_DISCOVERY_PORT)) {
        Ok(socket) => socket,
        Err(_) => return,
    };

    if std_socket
        .join_multicast_v4(&multicast, &Ipv4Addr::UNSPECIFIED)
        .is_err()
    {
        return;
    }

    if std_socket.set_nonblocking(true).is_err() {
        return;
    }

    let socket = match tokio::net::UdpSocket::from_std(std_socket) {
        Ok(socket) => socket,
        Err(_) => return,
    };

    let mut announce_interval = tokio::time::interval(Duration::from_secs(3));
    let mut idle_interval = tokio::time::interval(Duration::from_millis(250));
    let mut buffer = [0_u8; 2048];

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }

        tokio::select! {
            _ = announce_interval.tick() => {
                let message = LanAnnouncement {
                    v: 1,
                    npub: own_npub.clone(),
                    node_name: node_name.clone(),
                    endpoint: endpoint.clone(),
                    timestamp: unix_timestamp(),
                };

                if let Ok(encoded) = serde_json::to_vec(&message) {
                    let _ = socket.send_to(&encoded, target).await;
                }
            }
            recv = socket.recv_from(&mut buffer) => {
                if let Ok((len, _)) = recv
                    && let Ok(parsed) = serde_json::from_slice::<LanAnnouncement>(&buffer[..len])
                    && parsed.v == 1
                    && parsed.npub != own_npub
                {
                    let _ = tx.send(LanDiscoverySignal {
                        npub: parsed.npub,
                        node_name: parsed.node_name,
                        endpoint: parsed.endpoint,
                        seen_at: SystemTime::now(),
                    });
                }
            }
            _ = idle_interval.tick() => {}
        }
    }
}

struct AppState {
    backend: Mutex<NvpnBackend>,
}

fn with_backend<T>(
    state: State<'_, AppState>,
    f: impl FnOnce(&mut NvpnBackend) -> Result<T>,
) -> Result<T, String> {
    let mut backend = state
        .backend
        .lock()
        .map_err(|_| "backend lock poisoned".to_string())?;
    f(&mut backend).map_err(|error| error.to_string())
}

#[tauri::command]
fn tick(state: State<'_, AppState>) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[tauri::command]
fn connect_session(state: State<'_, AppState>) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.connect_session()?;
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[tauri::command]
fn disconnect_session(state: State<'_, AppState>) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.disconnect_session();
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[tauri::command]
fn add_participant(state: State<'_, AppState>, npub: String) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.add_participant(&npub)?;
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[tauri::command]
fn remove_participant(state: State<'_, AppState>, npub: String) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.remove_participant(&npub)?;
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[tauri::command]
fn add_relay(state: State<'_, AppState>, relay: String) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.add_relay(&relay)?;
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[tauri::command]
fn remove_relay(state: State<'_, AppState>, relay: String) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.remove_relay(&relay)?;
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[tauri::command]
fn update_settings(state: State<'_, AppState>, patch: SettingsPatch) -> Result<UiState, String> {
    with_backend(state, |backend| {
        backend.update_settings(patch)?;
        backend.tick();
        Ok(backend.ui_state())
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

    let backend = NvpnBackend::new().expect("failed to initialize GUI backend state");

    tauri::Builder::default()
        .manage(AppState {
            backend: Mutex::new(backend),
        })
        .invoke_handler(tauri::generate_handler![
            tick,
            connect_session,
            disconnect_session,
            add_participant,
            remove_participant,
            add_relay,
            remove_relay,
            update_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{connected_configured_peer_count, expected_peer_count, is_mesh_complete};
    use std::collections::HashMap;

    use crate::PeerLinkStatus;
    use nostr_vpn_core::config::AppConfig;

    #[test]
    fn expected_peer_count_excludes_own_participant_when_present() {
        let mut config = AppConfig::generated();
        config.participants = vec!["aa".to_string(), "bb".to_string(), "cc".to_string()];

        assert_eq!(expected_peer_count(&config), 3);
    }

    #[test]
    fn connected_peer_count_only_counts_reachable() {
        let mut config = AppConfig::generated();
        config.participants = vec!["aa".to_string(), "bb".to_string()];

        let mut map = HashMap::new();
        map.insert(
            "aa".to_string(),
            PeerLinkStatus {
                reachable: Some(true),
                ..PeerLinkStatus::default()
            },
        );
        map.insert(
            "bb".to_string(),
            PeerLinkStatus {
                reachable: Some(false),
                ..PeerLinkStatus::default()
            },
        );

        assert_eq!(connected_configured_peer_count(&config, &map), 1);
    }

    #[test]
    fn mesh_completion_requires_expected_non_zero() {
        assert!(!is_mesh_complete(0, 0));
        assert!(!is_mesh_complete(1, 2));
        assert!(is_mesh_complete(2, 2));
    }
}
