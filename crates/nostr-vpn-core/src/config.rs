use std::fs;
use std::net::{IpAddr, UdpSocket};
use std::path::Path;

use anyhow::{Context, Result};
use nostr_sdk::prelude::{Keys, PublicKey, ToBech32};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::crypto::generate_keypair;

/// Same defaults as hashtree's `DEFAULT_RELAYS`.
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://temp.iris.to",
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://offchain.pub",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NostrConfig {
    #[serde(default = "default_relays")]
    pub relays: Vec<String>,
    /// Nostr private identity key in `nsec` or hex format.
    #[serde(default)]
    pub secret_key: String,
    /// Nostr public identity key in `npub` or hex format.
    #[serde(default)]
    pub public_key: String,
}

impl Default for NostrConfig {
    fn default() -> Self {
        let (secret_key, public_key) = generate_nostr_identity();
        Self {
            relays: default_relays(),
            secret_key,
            public_key,
        }
    }
}

fn default_relays() -> Vec<String> {
    DEFAULT_RELAYS
        .iter()
        .map(|relay| relay.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_network_id")]
    pub network_id: String,
    #[serde(default = "default_node_name")]
    pub node_name: String,
    #[serde(default = "default_auto_disconnect_relays_when_mesh_ready")]
    pub auto_disconnect_relays_when_mesh_ready: bool,
    #[serde(default = "default_lan_discovery_enabled")]
    pub lan_discovery_enabled: bool,
    #[serde(default = "default_close_to_tray_on_close")]
    pub close_to_tray_on_close: bool,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub nostr: NostrConfig,
    #[serde(default)]
    pub node: NodeConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut config = Self {
            network_id: default_network_id(),
            node_name: default_node_name(),
            auto_disconnect_relays_when_mesh_ready: default_auto_disconnect_relays_when_mesh_ready(
            ),
            lan_discovery_enabled: default_lan_discovery_enabled(),
            close_to_tray_on_close: default_close_to_tray_on_close(),
            participants: Vec::new(),
            nostr: NostrConfig::default(),
            node: NodeConfig::default(),
        };
        config.ensure_defaults();
        config
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    #[serde(default = "default_node_id")]
    pub id: String,
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_tunnel_ip")]
    pub tunnel_ip: String,
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
}

impl Default for NodeConfig {
    fn default() -> Self {
        let key_pair = generate_keypair();
        Self {
            id: default_node_id(),
            private_key: key_pair.private_key,
            public_key: key_pair.public_key,
            endpoint: default_endpoint(),
            tunnel_ip: default_tunnel_ip(),
            listen_port: default_listen_port(),
        }
    }
}

impl AppConfig {
    pub fn generated() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let mut config: AppConfig =
            toml::from_str(&raw).with_context(|| "failed to parse config TOML")?;
        config.ensure_defaults();
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut to_write = self.clone();
        to_write.ensure_defaults();

        let raw = toml::to_string_pretty(&to_write).with_context(|| "failed to encode TOML")?;
        fs::write(path, raw).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    pub fn ensure_defaults(&mut self) {
        if self.node_name.trim().is_empty() {
            self.node_name = default_node_name();
        }

        if self.network_id.trim().is_empty() {
            self.network_id = default_network_id();
        }

        if self.nostr.relays.is_empty() {
            self.nostr.relays = default_relays();
        }

        if self.node.id.trim().is_empty() {
            self.node.id = default_node_id();
        }

        if self.node.endpoint.trim().is_empty() {
            self.node.endpoint = default_endpoint();
        }

        if self.node.tunnel_ip.trim().is_empty() {
            self.node.tunnel_ip = default_tunnel_ip();
        }

        if self.node.listen_port == 0 {
            self.node.listen_port = default_listen_port();
        }

        if self.node.private_key.trim().is_empty() || self.node.public_key.trim().is_empty() {
            let key_pair = generate_keypair();
            self.node.private_key = key_pair.private_key;
            self.node.public_key = key_pair.public_key;
        }

        self.ensure_nostr_identity();

        self.participants = self
            .participants
            .iter()
            .filter_map(|participant| normalize_nostr_pubkey(participant).ok())
            .collect();
        self.participants.sort();
        self.participants.dedup();
    }

    pub fn effective_network_id(&self) -> String {
        if self.participants.is_empty() {
            return self.network_id.clone();
        }

        let mesh_members = self.mesh_members_pubkeys();
        if mesh_members.is_empty() {
            return self.network_id.clone();
        }

        derive_network_id_from_participants(&mesh_members)
    }

    pub fn participant_pubkeys_hex(&self) -> Vec<String> {
        self.participants.clone()
    }

    pub fn mesh_members_pubkeys(&self) -> Vec<String> {
        let mut members = self.participant_pubkeys_hex();
        if let Ok(own_pubkey) = self.own_nostr_pubkey_hex() {
            members.push(own_pubkey);
        }
        members.sort();
        members.dedup();
        members
    }

    pub fn nostr_keys(&self) -> Result<Keys> {
        Keys::parse(&self.nostr.secret_key).context("invalid nostr secret key")
    }

    pub fn own_nostr_pubkey_hex(&self) -> Result<String> {
        normalize_nostr_pubkey(&self.nostr.public_key)
            .or_else(|_| self.nostr_keys().map(|keys| keys.public_key().to_hex()))
    }

    fn ensure_nostr_identity(&mut self) {
        if self.nostr.secret_key.trim().is_empty() {
            let (secret_key, public_key) = generate_nostr_identity();
            self.nostr.secret_key = secret_key;
            self.nostr.public_key = public_key;
            return;
        }

        if let Ok(keys) = Keys::parse(&self.nostr.secret_key) {
            if self.nostr.public_key.trim().is_empty() {
                self.nostr.public_key = keys
                    .public_key()
                    .to_bech32()
                    .unwrap_or_else(|_| keys.public_key().to_hex());
            }
            return;
        }

        let (secret_key, public_key) = generate_nostr_identity();
        self.nostr.secret_key = secret_key;
        self.nostr.public_key = public_key;
    }
}

pub fn derive_network_id_from_participants(participants: &[String]) -> String {
    let mut normalized: Vec<String> = participants.to_vec();
    normalized.sort();
    normalized.dedup();

    let mut hasher = Sha256::new();
    for participant in normalized {
        hasher.update(participant.as_bytes());
        hasher.update(b"\n");
    }

    let digest = hasher.finalize();
    format!("nostr-vpn:{}", &hex::encode(digest)[..16])
}

pub fn normalize_nostr_pubkey(value: &str) -> Result<String> {
    PublicKey::parse(value)
        .map(|public_key| public_key.to_hex())
        .map_err(|error| anyhow::anyhow!("invalid participant pubkey '{value}': {error}"))
}

pub fn maybe_autoconfigure_node(config: &mut AppConfig) {
    if needs_endpoint_autoconfig(&config.node.endpoint)
        && let Some(ip) = detect_primary_ipv4()
    {
        config.node.endpoint = format!("{ip}:{}", config.node.listen_port);
    }

    let mesh_members = config.mesh_members_pubkeys();
    if needs_tunnel_ip_autoconfig(&config.node.tunnel_ip)
        && let Ok(own_pubkey) = config.own_nostr_pubkey_hex()
        && let Some(tunnel_ip) = derive_mesh_tunnel_ip(&mesh_members, &own_pubkey)
    {
        config.node.tunnel_ip = tunnel_ip;
    }
}

pub fn derive_mesh_tunnel_ip(participants: &[String], own_pubkey_hex: &str) -> Option<String> {
    if participants.is_empty() {
        return None;
    }

    let mut normalized = participants.to_vec();
    normalized.sort();
    normalized.dedup();

    let host_octet = if let Some(index) = normalized.iter().position(|key| key == own_pubkey_hex) {
        ((index % 250) + 1) as u8
    } else {
        let digest = Sha256::digest(own_pubkey_hex.as_bytes());
        (digest[0] % 241) + 10
    };

    Some(format!("10.44.0.{host_octet}/32"))
}

pub fn needs_endpoint_autoconfig(endpoint: &str) -> bool {
    let value = endpoint.trim();
    if value.is_empty() {
        return true;
    }

    let host = value
        .rsplit_once(':')
        .map_or(value, |(host, _port)| host)
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');

    matches!(host, "127.0.0.1" | "0.0.0.0" | "localhost" | "::1")
}

pub fn needs_tunnel_ip_autoconfig(tunnel_ip: &str) -> bool {
    let value = tunnel_ip.trim();
    value.is_empty() || value == "10.44.0.1/32"
}

fn detect_primary_ipv4() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_ipv4() { Some(ip) } else { None }
}

fn generate_nostr_identity() -> (String, String) {
    let keys = Keys::generate();

    let secret_key = keys
        .secret_key()
        .to_bech32()
        .unwrap_or_else(|_| keys.secret_key().to_secret_hex());

    let public_key = keys
        .public_key()
        .to_bech32()
        .unwrap_or_else(|_| keys.public_key().to_hex());

    (secret_key, public_key)
}

fn default_network_id() -> String {
    "nostr-vpn".to_string()
}

fn default_node_name() -> String {
    "nostr-vpn-node".to_string()
}

const fn default_auto_disconnect_relays_when_mesh_ready() -> bool {
    true
}

const fn default_lan_discovery_enabled() -> bool {
    true
}

const fn default_close_to_tray_on_close() -> bool {
    true
}

fn default_node_id() -> String {
    Uuid::new_v4().to_string()
}

fn default_endpoint() -> String {
    "127.0.0.1:51820".to_string()
}

fn default_tunnel_ip() -> String {
    "10.44.0.1/32".to_string()
}

const fn default_listen_port() -> u16 {
    51820
}
