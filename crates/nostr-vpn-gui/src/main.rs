use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use eframe::egui;
use nostr_vpn_core::config::{AppConfig, normalize_nostr_pubkey};
use nostr_vpn_core::control::{PeerAnnouncement, PeerDirectory};
use nostr_vpn_core::signaling::{NostrSignalingClient, SignalEnvelope, SignalPayload};
use tokio::runtime::Runtime;

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

struct NostrVpnGui {
    runtime: Runtime,
    config_path: PathBuf,
    config: AppConfig,
    status: String,
    connected: bool,
    peers: PeerDirectory,
    client: Option<Arc<NostrSignalingClient>>,
    signal_rx: Option<mpsc::Receiver<SignalEnvelope>>,
    show_settings: bool,
    participant_input: String,
    relay_add_input: String,
    relay_status: HashMap<String, RelayStatus>,
    relay_check_rx: Option<mpsc::Receiver<Vec<RelayCheckResult>>>,
    relay_check_inflight: bool,
    next_relay_check_at: Option<Instant>,
    seen_participant_pubkeys: HashSet<String>,
    brand_texture: Option<egui::TextureHandle>,
}

impl NostrVpnGui {
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

        let relay_status = config
            .nostr
            .relays
            .iter()
            .map(|relay| (relay.clone(), RelayStatus::default()))
            .collect();

        Ok(Self {
            runtime,
            config_path,
            config,
            status: "Disconnected".to_string(),
            connected: false,
            peers: PeerDirectory::default(),
            client: None,
            signal_rx: None,
            show_settings: false,
            participant_input: String::new(),
            relay_add_input: String::new(),
            relay_status,
            relay_check_rx: None,
            relay_check_inflight: false,
            next_relay_check_at: None,
            seen_participant_pubkeys: HashSet::new(),
            brand_texture: None,
        }
        .with_inputs_from_config())
    }

    fn with_inputs_from_config(mut self) -> Self {
        self.participant_input = self.config.participants.join("\n");
        self
    }

    fn ensure_brand_texture(&mut self, ctx: &egui::Context) {
        if self.brand_texture.is_some() {
            return;
        }

        let icon = build_app_icon_data(128);
        let image: egui::ColorImage = (&icon).into();
        self.brand_texture =
            Some(ctx.load_texture("nvpn-brand", image, egui::TextureOptions::LINEAR));
    }

    fn connect(&mut self) {
        if self.connected {
            self.status = "Already connected".to_string();
            return;
        }

        match self.connect_inner() {
            Ok(()) => {
                self.connected = true;
                self.status = format!("Connected to {} relays", self.config.nostr.relays.len());
                self.start_relay_check(4);
                self.next_relay_check_at = Some(Instant::now() + Duration::from_secs(45));
            }
            Err(error) => {
                self.status = format!("Connect failed: {error}");
            }
        }
    }

    fn connect_inner(&mut self) -> Result<()> {
        if self.config.nostr.relays.is_empty() {
            return Err(anyhow!("at least one relay is required"));
        }

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
        self.peers = PeerDirectory::default();
        self.seen_participant_pubkeys.clear();

        Ok(())
    }

    fn disconnect(&mut self) {
        if !self.connected {
            return;
        }

        if let Some(client) = self.client.take() {
            self.runtime.block_on(client.disconnect());
        }

        self.signal_rx = None;
        self.connected = false;
        self.relay_check_inflight = false;
        self.relay_check_rx = None;
        self.next_relay_check_at = None;
        self.status = "Disconnected".to_string();
    }

    fn announce_now(&mut self) {
        let Some(client) = self.client.clone() else {
            self.status = "Connect first, then announce".to_string();
            return;
        };

        let announcement = PeerAnnouncement {
            node_id: self.config.node.id.clone(),
            public_key: self.config.node.public_key.clone(),
            endpoint: self.config.node.endpoint.clone(),
            tunnel_ip: self.config.node.tunnel_ip.clone(),
            timestamp: unix_timestamp(),
        };

        match self
            .runtime
            .block_on(client.publish(SignalPayload::Announce(announcement)))
        {
            Ok(()) => {
                self.status = "Announcement sent".to_string();
            }
            Err(error) => {
                self.status = format!("Announcement failed: {error}");
            }
        }
    }

    fn start_relay_check(&mut self, timeout_secs: u64) {
        self.ensure_relay_status_entries();

        if self.relay_check_inflight || self.config.nostr.relays.is_empty() {
            return;
        }

        for relay in &self.config.nostr.relays {
            self.relay_status
                .entry(relay.clone())
                .and_modify(|status| status.checking = true)
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
        if !self.connected || self.relay_check_inflight {
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

    fn handle_signals(&mut self) {
        let mut pending = Vec::new();
        if let Some(rx) = &self.signal_rx {
            while let Ok(message) = rx.try_recv() {
                pending.push(message);
            }
        }

        for message in pending {
            let sender_pubkey = message.sender_pubkey;
            match message.payload {
                SignalPayload::Announce(announcement) => {
                    self.peers.apply(announcement);
                    self.seen_participant_pubkeys.insert(sender_pubkey);
                }
                SignalPayload::Disconnect { node_id } => {
                    self.peers.remove(&node_id);
                    self.seen_participant_pubkeys.remove(&sender_pubkey);
                }
            }
        }

        self.maybe_auto_disconnect_relays();
    }

    fn maybe_auto_disconnect_relays(&mut self) {
        if !self.connected || !self.config.auto_disconnect_relays_when_mesh_ready {
            return;
        }

        let expected = expected_peer_count(&self.config);
        let discovered =
            discovered_participant_count(&self.seen_participant_pubkeys, &self.config.participants);

        if is_mesh_complete(discovered, expected) {
            self.disconnect();
            self.status =
                format!("Mesh complete ({discovered}/{expected}) - relay connections paused");
        }
    }

    fn add_relay_from_input(&mut self) {
        let relay = self.relay_add_input.trim().to_string();
        if relay.is_empty() {
            self.status = "Relay URL is empty".to_string();
            return;
        }

        if !(relay.starts_with("ws://") || relay.starts_with("wss://")) {
            self.status = "Relay URL must start with ws:// or wss://".to_string();
            return;
        }

        if self
            .config
            .nostr
            .relays
            .iter()
            .any(|existing| existing == &relay)
        {
            self.status = "Relay already exists".to_string();
            return;
        }

        self.config.nostr.relays.push(relay.clone());
        self.relay_status.entry(relay).or_default();
        self.relay_add_input.clear();
        self.status = "Relay added (save to persist)".to_string();
    }

    fn remove_relay(&mut self, index: usize) {
        if self.config.nostr.relays.len() <= 1 {
            self.status = "At least one relay is required".to_string();
            return;
        }

        if index >= self.config.nostr.relays.len() {
            return;
        }

        let relay = self.config.nostr.relays.remove(index);
        self.relay_status.remove(&relay);
        self.status = "Relay removed (save to persist)".to_string();
    }

    fn save_settings(&mut self) {
        if self.config.nostr.relays.is_empty() {
            self.status = "At least one relay is required".to_string();
            return;
        }

        let mut participants = Vec::new();
        for participant in self
            .participant_input
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            match normalize_nostr_pubkey(participant) {
                Ok(pubkey) => participants.push(pubkey),
                Err(error) => {
                    self.status = format!("Invalid participant '{participant}': {error}");
                    return;
                }
            }
        }

        participants.sort();
        participants.dedup();
        self.config.participants = participants;
        self.config.ensure_defaults();

        if let Err(error) = self.config.save(&self.config_path) {
            self.status = format!("Failed to save settings: {error}");
            return;
        }

        self.ensure_relay_status_entries();
        self.status = format!("Saved {}", self.config_path.display());
    }

    fn ensure_relay_status_entries(&mut self) {
        let configured: HashSet<String> = self.config.nostr.relays.iter().cloned().collect();
        self.relay_status
            .retain(|relay, _| configured.contains(relay));
        for relay in &self.config.nostr.relays {
            self.relay_status.entry(relay.clone()).or_default();
        }
    }

    fn relay_summary(&self) -> (usize, usize, usize) {
        let mut up = 0;
        let mut down = 0;
        let mut checking_or_unknown = 0;

        for relay in &self.config.nostr.relays {
            match self.relay_status.get(relay) {
                Some(status) if status.checking => checking_or_unknown += 1,
                Some(status) if status.error.is_none() && status.latency_ms.is_some() => up += 1,
                Some(status) if status.error.is_some() => down += 1,
                _ => checking_or_unknown += 1,
            }
        }

        (up, down, checking_or_unknown)
    }

    fn relay_status_line(&self, relay: &str) -> String {
        let Some(status) = self.relay_status.get(relay) else {
            return "unknown".to_string();
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

        "unknown".to_string()
    }
}

impl eframe::App for NostrVpnGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_brand_texture(ctx);
        self.ensure_relay_status_entries();
        self.handle_relay_checks();
        self.handle_signals();
        self.maybe_schedule_periodic_relay_check();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(texture) = &self.brand_texture {
                    ui.image((texture.id(), egui::vec2(18.0, 18.0)));
                }
                ui.label(egui::RichText::new("nvpn").strong());
                ui.separator();

                ui.menu_button("Menu", |ui| {
                    if ui.button("Connect").clicked() {
                        self.connect();
                        ui.close();
                    }
                    if ui.button("Disconnect").clicked() {
                        self.disconnect();
                        ui.close();
                    }
                    if ui.button("Announce").clicked() {
                        self.announce_now();
                        ui.close();
                    }
                    if ui.button("Check Relays").clicked() {
                        self.start_relay_check(4);
                        ui.close();
                    }
                    if ui.button("Settings").clicked() {
                        self.show_settings = true;
                        ui.close();
                    }
                    if ui.button("Quit").clicked() {
                        self.disconnect();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                });

                if ui.button("Connect").clicked() {
                    self.connect();
                }
                if ui.button("Disconnect").clicked() {
                    self.disconnect();
                }
                if ui.button("Announce").clicked() {
                    self.announce_now();
                }
                if ui.button("Check Relays").clicked() {
                    self.start_relay_check(4);
                }
                if ui.button("Settings").clicked() {
                    self.show_settings = !self.show_settings;
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(texture) = &self.brand_texture {
                    ui.image((texture.id(), egui::vec2(30.0, 30.0)));
                }
                ui.heading("Nostr VPN");
            });

            ui.label(format!("Network: {}", self.config.effective_network_id()));
            ui.label(format!("Node ID: {}", self.config.node.id));
            ui.label(format!("Nostr Pubkey: {}", self.config.nostr.public_key));
            ui.label(format!("Status: {}", self.status));

            let (up, down, pending) = self.relay_summary();
            ui.horizontal(|ui| {
                ui.label("Relay Health:");
                ui.colored_label(egui::Color32::from_rgb(72, 199, 116), format!("{up} up"));
                ui.colored_label(egui::Color32::from_rgb(239, 83, 80), format!("{down} down"));
                ui.colored_label(
                    egui::Color32::from_rgb(255, 193, 7),
                    format!("{pending} pending"),
                );
            });

            let expected = expected_peer_count(&self.config);
            let discovered = discovered_participant_count(
                &self.seen_participant_pubkeys,
                &self.config.participants,
            );
            if expected > 0 {
                ui.label(format!(
                    "Participant mesh progress: {discovered}/{expected}"
                ));
                if self.config.auto_disconnect_relays_when_mesh_ready {
                    ui.small("Auto-disconnect is enabled when mesh is complete.");
                }
            }

            ui.separator();
            ui.label("Discovered Peers");
            let peers = self.peers.all();
            if peers.is_empty() {
                ui.label("No peers discovered yet.");
            } else {
                egui::Grid::new("peer-grid").striped(true).show(ui, |ui| {
                    ui.strong("Node");
                    ui.strong("Endpoint");
                    ui.strong("Tunnel IP");
                    ui.end_row();

                    for peer in peers {
                        ui.label(peer.node_id);
                        ui.label(peer.endpoint);
                        ui.label(peer.tunnel_ip);
                        ui.end_row();
                    }
                });
            }
        });

        if self.show_settings {
            let mut open = self.show_settings;
            let mut save_clicked = false;
            let mut close_clicked = false;
            let mut relay_to_remove = None;

            egui::Window::new("Settings")
                .open(&mut open)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label(format!("Config: {}", self.config_path.display()));
                    ui.horizontal(|ui| {
                        ui.label("Fallback Network ID");
                        ui.text_edit_singleline(&mut self.config.network_id);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Node Name");
                        ui.text_edit_singleline(&mut self.config.node_name);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Endpoint");
                        ui.text_edit_singleline(&mut self.config.node.endpoint);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Tunnel IP");
                        ui.text_edit_singleline(&mut self.config.node.tunnel_ip);
                    });
                    ui.checkbox(
                        &mut self.config.auto_disconnect_relays_when_mesh_ready,
                        "Auto-disconnect relays when all configured participants are discovered",
                    );

                    ui.separator();
                    ui.label("Participants (npub/hex, one per line)");
                    ui.text_edit_multiline(&mut self.participant_input);

                    ui.separator();
                    ui.heading("Relays");
                    ui.horizontal(|ui| {
                        ui.label("Add Relay");
                        ui.text_edit_singleline(&mut self.relay_add_input);
                        if ui.button("Add").clicked() {
                            self.add_relay_from_input();
                        }
                        if ui.button("Check Status").clicked() {
                            self.start_relay_check(4);
                        }
                    });

                    if self.config.nostr.relays.is_empty() {
                        ui.colored_label(
                            egui::Color32::from_rgb(239, 83, 80),
                            "No relays configured",
                        );
                    }

                    for (index, relay) in self.config.nostr.relays.iter().enumerate() {
                        let indicator = relay_indicator_color(self.relay_status.get(relay));
                        ui.horizontal(|ui| {
                            ui.colored_label(indicator, "●");
                            ui.monospace(relay);
                            ui.small(self.relay_status_line(relay));
                            if ui.button("Remove").clicked() {
                                relay_to_remove = Some(index);
                            }
                        });
                    }

                    if self.relay_check_inflight {
                        ui.small("Relay check in progress...");
                    }

                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            save_clicked = true;
                        }
                        if ui.button("Close").clicked() {
                            close_clicked = true;
                        }
                    });
                });

            if let Some(index) = relay_to_remove {
                self.remove_relay(index);
            }
            if save_clicked {
                self.save_settings();
            }
            if close_clicked {
                open = false;
            }
            self.show_settings = open;
        }

        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

fn relay_indicator_color(status: Option<&RelayStatus>) -> egui::Color32 {
    match status {
        Some(state) if state.checking => egui::Color32::from_rgb(255, 193, 7),
        Some(state) if state.error.is_none() && state.latency_ms.is_some() => {
            egui::Color32::from_rgb(72, 199, 116)
        }
        Some(state) if state.error.is_some() => egui::Color32::from_rgb(239, 83, 80),
        _ => egui::Color32::from_rgb(158, 158, 158),
    }
}

fn expected_peer_count(config: &AppConfig) -> usize {
    let own_pubkey = config.own_nostr_pubkey_hex().ok();
    expected_peer_count_from_parts(&config.participants, own_pubkey.as_deref())
}

fn expected_peer_count_from_parts(participants: &[String], own_pubkey: Option<&str>) -> usize {
    if participants.is_empty() {
        return 0;
    }

    let mut expected = participants.len();
    if let Some(own_pubkey) = own_pubkey
        && participants
            .iter()
            .any(|participant| participant == own_pubkey)
    {
        expected = expected.saturating_sub(1);
    }

    expected
}

fn discovered_participant_count(discovered: &HashSet<String>, configured: &[String]) -> usize {
    if configured.is_empty() {
        return discovered.len();
    }

    discovered
        .iter()
        .filter(|pubkey| configured.iter().any(|participant| participant == *pubkey))
        .count()
}

fn is_mesh_complete(discovered: usize, expected: usize) -> bool {
    expected > 0 && discovered >= expected
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

fn build_app_icon_data(size: u32) -> egui::IconData {
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let max = (size.saturating_sub(1)) as f32;

    for y in 0..size {
        for x in 0..size {
            let nx = ((x as f32 / max) * 2.0) - 1.0;
            let ny = ((y as f32 / max) * 2.0) - 1.0;
            let dist = (nx * nx + ny * ny).sqrt();

            if dist > 0.96 {
                continue;
            }

            let grad = ((ny + 1.0) * 0.5).clamp(0.0, 1.0);
            let glow = (1.0 - (dist / 0.96)).clamp(0.0, 1.0);
            let mut r = (10.0 + (70.0 * grad) + (35.0 * glow)).clamp(0.0, 255.0) as u8;
            let mut g = (26.0 + (170.0 * grad) + (25.0 * glow)).clamp(0.0, 255.0) as u8;
            let mut b = (58.0 + (155.0 * grad) + (10.0 * glow)).clamp(0.0, 255.0) as u8;
            let mut a = 255_u8;

            let ring = dist > 0.68 && dist < 0.78;
            if ring {
                r = r.saturating_add(35);
                g = g.saturating_add(25);
                b = b.saturating_add(10);
            }

            let shield_width = 0.58 - ((ny + 0.62).max(0.0) * 0.38);
            let in_shield = ny > -0.62 && ny < 0.62 && nx.abs() < shield_width;
            if in_shield {
                r = (r as f32 * 0.55) as u8;
                g = (g as f32 * 0.6) as u8;
                b = (b as f32 * 0.72) as u8;
                a = 245;
            }

            let nodes = [
                (0.0_f32, -0.28_f32),
                (-0.27_f32, 0.11_f32),
                (0.27_f32, 0.11_f32),
                (0.0_f32, 0.43_f32),
            ];
            let edges = [(0, 1), (0, 2), (1, 3), (2, 3)];

            let mut on_edge = false;
            for (start, end) in edges {
                let (ax, ay) = nodes[start];
                let (bx, by) = nodes[end];
                let edge_dist = distance_to_segment(nx, ny, ax, ay, bx, by);
                if edge_dist < 0.036 {
                    on_edge = true;
                    break;
                }
            }

            if on_edge {
                r = 214;
                g = 243;
                b = 255;
                a = 255;
            }

            for (cx, cy) in nodes {
                let dx = nx - cx;
                let dy = ny - cy;
                if (dx * dx + dy * dy).sqrt() < 0.085 {
                    r = 236;
                    g = 250;
                    b = 255;
                    a = 255;
                    break;
                }
            }

            let index = ((y * size + x) * 4) as usize;
            rgba[index] = r;
            rgba[index + 1] = g;
            rgba[index + 2] = b;
            rgba[index + 3] = a;
        }
    }

    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

fn distance_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let vx = bx - ax;
    let vy = by - ay;
    let wx = px - ax;
    let wy = py - ay;

    let len_sq = vx * vx + vy * vy;
    if len_sq <= f32::EPSILON {
        return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
    }

    let t = ((wx * vx + wy * vy) / len_sq).clamp(0.0, 1.0);
    let proj_x = ax + t * vx;
    let proj_y = ay + t * vy;
    ((px - proj_x).powi(2) + (py - proj_y).powi(2)).sqrt()
}

fn main() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nostr VPN")
            .with_inner_size([460.0, 560.0])
            .with_icon(build_app_icon_data(256)),
        ..Default::default()
    };

    let app = NostrVpnGui::new()?;

    eframe::run_native(
        "Nostr VPN",
        options,
        Box::new(move |_creation_context| Ok(Box::new(app))),
    )
    .map_err(|error| anyhow!("failed to run GUI: {error}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        build_app_icon_data, discovered_participant_count, expected_peer_count_from_parts,
        is_mesh_complete,
    };

    #[test]
    fn expected_peer_count_excludes_own_participant() {
        let participants = vec!["aa".to_string(), "bb".to_string(), "cc".to_string()];
        let expected = expected_peer_count_from_parts(&participants, Some("bb"));
        assert_eq!(expected, 2);
    }

    #[test]
    fn participant_discovery_counts_only_configured_members() {
        let configured = vec!["aa".to_string(), "bb".to_string()];
        let discovered = HashSet::from(["aa".to_string(), "bb".to_string(), "extra".to_string()]);

        assert_eq!(discovered_participant_count(&discovered, &configured), 2);
    }

    #[test]
    fn mesh_completion_requires_expected_non_zero() {
        assert!(!is_mesh_complete(0, 0));
        assert!(!is_mesh_complete(1, 2));
        assert!(is_mesh_complete(2, 2));
    }

    #[test]
    fn icon_data_is_square_and_non_empty() {
        let icon = build_app_icon_data(64);
        assert_eq!(icon.width, 64);
        assert_eq!(icon.height, 64);
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
        assert!(icon.rgba.iter().any(|channel| *channel > 0));
    }
}
