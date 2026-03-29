use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use nostr_vpn_core::nat::{
    DISCOVER_REQUEST_PREFIX, ENDPOINT_RESPONSE_PREFIX, PUNCH_ACK_PREFIX, PUNCH_REQUEST_PREFIX,
};
use nostr_vpn_core::node_record::{
    NODE_RECORD_D_TAG, NodeRecord, NodeRecordMode, NodeService, NodeServiceKind,
    publish_node_record,
};
use nostr_vpn_core::relay::{
    NatAssistOperatorState, RelayAllocationGranted, RelayOperatorSessionState, RelayOperatorState,
    ServiceOperatorState,
};
use nostr_vpn_core::service_signaling::{RelayServiceClient, ServicePayload};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::Instant;

const DEFAULT_LEASE_SECS: u64 = 120;
const DEFAULT_PUBLISH_INTERVAL_SECS: u64 = 30;
const DEFAULT_NAT_ASSIST_PORT: u16 = 3478;
const DEFAULT_STATE_FILE_NAME: &str = "relay.operator.json";
const STATE_WRITE_INTERVAL_SECS: u64 = 1;

#[derive(Debug, Parser)]
#[command(name = "nvpn-udp-relay")]
#[command(about = "Experimental public UDP services for nostr-vpn")]
struct Args {
    #[arg(long)]
    secret_key: String,
    #[arg(long = "relay")]
    relays: Vec<String>,
    #[arg(long, default_value = "0.0.0.0")]
    bind_ip: String,
    #[arg(long)]
    advertise_host: String,
    #[arg(long, default_value_t = false)]
    disable_relay: bool,
    #[arg(long, default_value_t = false)]
    enable_nat_assist: bool,
    #[arg(long, default_value_t = DEFAULT_NAT_ASSIST_PORT)]
    nat_assist_port: u16,
    #[arg(long, default_value_t = DEFAULT_LEASE_SECS)]
    lease_secs: u64,
    #[arg(long, default_value_t = DEFAULT_PUBLISH_INTERVAL_SECS)]
    publish_interval_secs: u64,
    #[arg(long)]
    price_hint_msats: Option<u64>,
    #[arg(long)]
    state_file: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct SessionLeg {
    bound_addr: Option<SocketAddr>,
}

#[derive(Debug, Default)]
struct SessionState {
    requester: SessionLeg,
    target: SessionLeg,
}

#[derive(Debug, Default)]
struct RelayRuntimeState {
    relay_pubkey: String,
    advertised_endpoint: String,
    total_sessions_served: u64,
    total_forwarded_bytes: u64,
    current_forward_bps: u64,
    last_rate_sample_at: u64,
    last_rate_sample_bytes: u64,
    known_peer_pubkeys: HashSet<String>,
    active_sessions: HashMap<String, RelayOperatorSessionState>,
}

impl RelayRuntimeState {
    fn note_session_started(&mut self, session: RelayOperatorSessionState) {
        self.total_sessions_served = self.total_sessions_served.saturating_add(1);
        self.known_peer_pubkeys
            .insert(session.requester_pubkey.clone());
        self.known_peer_pubkeys
            .insert(session.target_pubkey.clone());
        self.active_sessions
            .insert(session.request_id.clone(), session);
    }

    fn note_forwarded_bytes(&mut self, request_id: &str, requester_leg: bool, bytes: u64) {
        self.total_forwarded_bytes = self.total_forwarded_bytes.saturating_add(bytes);
        if let Some(session) = self.active_sessions.get_mut(request_id) {
            if requester_leg {
                session.bytes_from_requester = session.bytes_from_requester.saturating_add(bytes);
            } else {
                session.bytes_from_target = session.bytes_from_target.saturating_add(bytes);
            }
        }
    }

    fn snapshot(&mut self, now: u64) -> RelayOperatorState {
        self.active_sessions
            .retain(|_, session| session.expires_at > now);

        let elapsed = now.saturating_sub(self.last_rate_sample_at);
        if elapsed > 0 {
            let bytes_delta = self
                .total_forwarded_bytes
                .saturating_sub(self.last_rate_sample_bytes);
            self.current_forward_bps = bytes_delta / elapsed;
            self.last_rate_sample_at = now;
            self.last_rate_sample_bytes = self.total_forwarded_bytes;
        } else if self.last_rate_sample_at == 0 {
            self.last_rate_sample_at = now;
            self.last_rate_sample_bytes = self.total_forwarded_bytes;
        }

        let mut known_peer_pubkeys = self.known_peer_pubkeys.iter().cloned().collect::<Vec<_>>();
        known_peer_pubkeys.sort();

        let mut active_sessions = self.active_sessions.values().cloned().collect::<Vec<_>>();
        active_sessions.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        });

        RelayOperatorState {
            updated_at: now,
            relay_pubkey: self.relay_pubkey.clone(),
            advertised_endpoint: self.advertised_endpoint.clone(),
            total_sessions_served: self.total_sessions_served,
            total_forwarded_bytes: self.total_forwarded_bytes,
            current_forward_bps: self.current_forward_bps,
            unique_peer_count: known_peer_pubkeys.len(),
            known_peer_pubkeys,
            active_sessions,
        }
    }
}

#[derive(Debug, Default)]
struct NatAssistRuntimeState {
    advertised_endpoint: String,
    total_discovery_requests: u64,
    total_punch_requests: u64,
    current_request_bps: u64,
    last_rate_sample_at: u64,
    last_rate_sample_requests: u64,
    known_clients: HashSet<String>,
}

impl NatAssistRuntimeState {
    fn note_discovery_request(&mut self, src: SocketAddr) {
        self.total_discovery_requests = self.total_discovery_requests.saturating_add(1);
        self.known_clients.insert(src.ip().to_string());
    }

    fn note_punch_request(&mut self, src: SocketAddr) {
        self.total_punch_requests = self.total_punch_requests.saturating_add(1);
        self.known_clients.insert(src.ip().to_string());
    }

    fn snapshot(&mut self, now: u64) -> NatAssistOperatorState {
        let total_requests = self
            .total_discovery_requests
            .saturating_add(self.total_punch_requests);
        let elapsed = now.saturating_sub(self.last_rate_sample_at);
        if elapsed > 0 {
            let requests_delta = total_requests.saturating_sub(self.last_rate_sample_requests);
            self.current_request_bps = requests_delta / elapsed;
            self.last_rate_sample_at = now;
            self.last_rate_sample_requests = total_requests;
        } else if self.last_rate_sample_at == 0 {
            self.last_rate_sample_at = now;
            self.last_rate_sample_requests = total_requests;
        }

        NatAssistOperatorState {
            updated_at: now,
            advertised_endpoint: self.advertised_endpoint.clone(),
            total_discovery_requests: self.total_discovery_requests,
            total_punch_requests: self.total_punch_requests,
            current_request_bps: self.current_request_bps,
            unique_client_count: self.known_clients.len(),
        }
    }
}

#[derive(Debug, Default)]
struct ServiceRuntimeState {
    operator_pubkey: String,
    relay: Option<RelayRuntimeState>,
    nat_assist: Option<NatAssistRuntimeState>,
}

impl ServiceRuntimeState {
    fn note_session_started(&mut self, session: RelayOperatorSessionState) {
        if let Some(relay) = self.relay.as_mut() {
            relay.note_session_started(session);
        }
    }

    fn note_forwarded_bytes(&mut self, request_id: &str, requester_leg: bool, bytes: u64) {
        if let Some(relay) = self.relay.as_mut() {
            relay.note_forwarded_bytes(request_id, requester_leg, bytes);
        }
    }

    fn note_discovery_request(&mut self, src: SocketAddr) {
        if let Some(nat_assist) = self.nat_assist.as_mut() {
            nat_assist.note_discovery_request(src);
        }
    }

    fn note_punch_request(&mut self, src: SocketAddr) {
        if let Some(nat_assist) = self.nat_assist.as_mut() {
            nat_assist.note_punch_request(src);
        }
    }

    fn snapshot(&mut self, now: u64) -> ServiceOperatorState {
        ServiceOperatorState {
            updated_at: now,
            operator_pubkey: self.operator_pubkey.clone(),
            relay: self.relay.as_mut().map(|relay| relay.snapshot(now)),
            nat_assist: self
                .nat_assist
                .as_mut()
                .map(|nat_assist| nat_assist.snapshot(now)),
        }
    }
}

fn relay_runtime_state_from_snapshot(
    snapshot: RelayOperatorState,
    relay_pubkey: String,
    advertised_endpoint: String,
    now: u64,
) -> RelayRuntimeState {
    RelayRuntimeState {
        relay_pubkey,
        advertised_endpoint,
        total_sessions_served: snapshot.total_sessions_served,
        total_forwarded_bytes: snapshot.total_forwarded_bytes,
        current_forward_bps: 0,
        last_rate_sample_at: now,
        last_rate_sample_bytes: snapshot.total_forwarded_bytes,
        known_peer_pubkeys: snapshot.known_peer_pubkeys.into_iter().collect(),
        active_sessions: HashMap::new(),
    }
}

fn nat_assist_runtime_state_from_snapshot(
    snapshot: NatAssistOperatorState,
    advertised_endpoint: String,
    now: u64,
) -> NatAssistRuntimeState {
    let total_requests = snapshot
        .total_discovery_requests
        .saturating_add(snapshot.total_punch_requests);
    NatAssistRuntimeState {
        advertised_endpoint,
        total_discovery_requests: snapshot.total_discovery_requests,
        total_punch_requests: snapshot.total_punch_requests,
        current_request_bps: 0,
        last_rate_sample_at: now,
        last_rate_sample_requests: total_requests,
        known_clients: HashSet::new(),
    }
}

fn load_runtime_state(
    path: &Path,
    operator_pubkey: String,
    relay_endpoint: Option<String>,
    nat_assist_endpoint: Option<String>,
) -> ServiceRuntimeState {
    let now = unix_timestamp();
    let Ok(raw) = fs::read(path) else {
        return ServiceRuntimeState {
            operator_pubkey: operator_pubkey.clone(),
            relay: relay_endpoint.map(|advertised_endpoint| RelayRuntimeState {
                relay_pubkey: operator_pubkey.clone(),
                advertised_endpoint,
                last_rate_sample_at: now,
                ..RelayRuntimeState::default()
            }),
            nat_assist: nat_assist_endpoint.map(|advertised_endpoint| NatAssistRuntimeState {
                advertised_endpoint,
                last_rate_sample_at: now,
                ..NatAssistRuntimeState::default()
            }),
        };
    };

    match serde_json::from_slice::<ServiceOperatorState>(&raw) {
        Ok(snapshot)
            if snapshot.relay.is_some()
                || snapshot.nat_assist.is_some()
                || !snapshot.operator_pubkey.trim().is_empty() =>
        {
            ServiceRuntimeState {
                operator_pubkey: operator_pubkey.clone(),
                relay: relay_endpoint.map(|advertised_endpoint| {
                    let advertised_endpoint_fallback = advertised_endpoint.clone();
                    snapshot.relay.map_or_else(
                        || RelayRuntimeState {
                            relay_pubkey: operator_pubkey.clone(),
                            advertised_endpoint: advertised_endpoint_fallback,
                            last_rate_sample_at: now,
                            ..RelayRuntimeState::default()
                        },
                        |relay| {
                            relay_runtime_state_from_snapshot(
                                relay,
                                operator_pubkey.clone(),
                                advertised_endpoint,
                                now,
                            )
                        },
                    )
                }),
                nat_assist: nat_assist_endpoint.map(|advertised_endpoint| {
                    let advertised_endpoint_fallback = advertised_endpoint.clone();
                    snapshot.nat_assist.map_or_else(
                        || NatAssistRuntimeState {
                            advertised_endpoint: advertised_endpoint_fallback,
                            last_rate_sample_at: now,
                            ..NatAssistRuntimeState::default()
                        },
                        |nat_assist| {
                            nat_assist_runtime_state_from_snapshot(
                                nat_assist,
                                advertised_endpoint,
                                now,
                            )
                        },
                    )
                }),
            }
        }
        Err(_) | Ok(_) => match serde_json::from_slice::<RelayOperatorState>(&raw) {
            Ok(snapshot) => ServiceRuntimeState {
                operator_pubkey: operator_pubkey.clone(),
                relay: relay_endpoint.map(|advertised_endpoint| {
                    relay_runtime_state_from_snapshot(
                        snapshot,
                        operator_pubkey.clone(),
                        advertised_endpoint,
                        now,
                    )
                }),
                nat_assist: nat_assist_endpoint.map(|advertised_endpoint| NatAssistRuntimeState {
                    advertised_endpoint,
                    last_rate_sample_at: now,
                    ..NatAssistRuntimeState::default()
                }),
            },
            Err(error) => {
                eprintln!(
                    "relay-service: failed to parse existing state file {}: {error}",
                    path.display()
                );
                ServiceRuntimeState {
                    operator_pubkey: operator_pubkey.clone(),
                    relay: relay_endpoint.map(|advertised_endpoint| RelayRuntimeState {
                        relay_pubkey: operator_pubkey.clone(),
                        advertised_endpoint,
                        last_rate_sample_at: now,
                        ..RelayRuntimeState::default()
                    }),
                    nat_assist: nat_assist_endpoint.map(|advertised_endpoint| {
                        NatAssistRuntimeState {
                            advertised_endpoint,
                            last_rate_sample_at: now,
                            ..NatAssistRuntimeState::default()
                        }
                    }),
                }
            }
        },
    }
}

fn node_record_services(args: &Args) -> Result<Vec<NodeService>> {
    let mut services = Vec::new();
    if !args.disable_relay {
        services.push(NodeService {
            kind: NodeServiceKind::Relay,
            endpoint: format!("{}:0", args.advertise_host),
            protocol: Some("udp-port-pair".to_string()),
            price_hint_msats: args.price_hint_msats,
        });
    }
    if args.enable_nat_assist {
        services.push(NodeService {
            kind: NodeServiceKind::NatAssist,
            endpoint: SocketAddr::new(
                parse_advertise_ip(&args.advertise_host)?,
                args.nat_assist_port,
            )
            .to_string(),
            protocol: Some("udp-reflector".to_string()),
            price_hint_msats: None,
        });
    }
    Ok(services)
}

fn spawn_node_record_publisher(args: &Args) -> Result<()> {
    let services = node_record_services(args)?;
    let secret_key = args.secret_key.clone();
    let relays = args.relays.clone();
    let publish_interval_secs = args.publish_interval_secs.max(5);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(publish_interval_secs));
        loop {
            interval.tick().await;
            let record = NodeRecord {
                mode: NodeRecordMode::PublicService,
                services: services.clone(),
                updated_at: unix_timestamp(),
                expires_at: unix_timestamp() + publish_interval_secs * 3,
            };
            if let Err(error) = publish_node_record(&secret_key, &relays, &record).await {
                eprintln!(
                    "relay-service: failed to publish {} record: {error}",
                    NODE_RECORD_D_TAG,
                );
            }
        }
    });
    Ok(())
}

fn spawn_nat_assist_listener(
    bind_ip: IpAddr,
    port: u16,
    service_runtime_state: Arc<Mutex<ServiceRuntimeState>>,
) {
    tokio::spawn(async move {
        let bind_addr = SocketAddr::new(bind_ip, port);
        let socket = match UdpSocket::bind(bind_addr).await {
            Ok(socket) => socket,
            Err(error) => {
                eprintln!("relay-service: failed to bind nat assist on {bind_addr}: {error}");
                return;
            }
        };
        let mut buf = [0u8; 2048];
        loop {
            let (read, src) = match socket.recv_from(&mut buf).await {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("relay-service: nat assist recv failed: {error}");
                    return;
                }
            };
            let payload = std::str::from_utf8(&buf[..read]).unwrap_or_default();
            if payload.starts_with(DISCOVER_REQUEST_PREFIX) {
                {
                    let mut stats = service_runtime_state.lock().await;
                    stats.note_discovery_request(src);
                }
                let response = format!("{ENDPOINT_RESPONSE_PREFIX} {src}");
                let _ = socket.send_to(response.as_bytes(), src).await;
                continue;
            }
            if payload.starts_with(PUNCH_REQUEST_PREFIX) {
                {
                    let mut stats = service_runtime_state.lock().await;
                    stats.note_punch_request(src);
                }
                let _ = socket.send_to(PUNCH_ACK_PREFIX.as_bytes(), src).await;
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.relays.is_empty() {
        return Err(anyhow!("at least one --relay is required"));
    }
    if args.disable_relay && !args.enable_nat_assist {
        return Err(anyhow!(
            "at least one public service must be enabled (relay or nat assist)"
        ));
    }

    let bind_ip = args
        .bind_ip
        .parse::<IpAddr>()
        .with_context(|| format!("invalid bind ip {}", args.bind_ip))?;
    let state_file = args
        .state_file
        .clone()
        .unwrap_or_else(default_state_file_path);
    let service_client = Arc::new(RelayServiceClient::from_secret_key(&args.secret_key)?);
    service_client.connect(&args.relays).await?;

    println!(
        "nvpn-udp-relay connected as {} on {} relays",
        service_client.own_pubkey(),
        args.relays.len()
    );

    let relay_endpoint = (!args.disable_relay).then(|| format!("{}:0", args.advertise_host));
    let nat_assist_endpoint = if args.enable_nat_assist {
        Some(
            SocketAddr::new(
                parse_advertise_ip(&args.advertise_host)?,
                args.nat_assist_port,
            )
            .to_string(),
        )
    } else {
        None
    };
    let service_runtime_state = Arc::new(Mutex::new(load_runtime_state(
        &state_file,
        service_client.own_pubkey().to_string(),
        relay_endpoint,
        nat_assist_endpoint,
    )));
    spawn_state_writer(state_file, service_runtime_state.clone());
    spawn_node_record_publisher(&args)?;
    if args.enable_nat_assist {
        spawn_nat_assist_listener(bind_ip, args.nat_assist_port, service_runtime_state.clone());
    }

    if args.disable_relay {
        std::future::pending::<()>().await;
        return Ok(());
    }

    loop {
        let Some(message) = service_client.recv().await else {
            break;
        };
        let ServicePayload::RelayAllocationRequest(request) = message.payload else {
            continue;
        };

        let requester_socket = Arc::new(
            UdpSocket::bind(SocketAddr::new(bind_ip, 0))
                .await
                .with_context(|| format!("failed to bind requester leg on {bind_ip}"))?,
        );
        let target_socket = Arc::new(
            UdpSocket::bind(SocketAddr::new(bind_ip, 0))
                .await
                .with_context(|| format!("failed to bind target leg on {bind_ip}"))?,
        );
        let requester_addr = requester_socket
            .local_addr()
            .context("failed to read requester leg addr")?;
        let target_addr = target_socket
            .local_addr()
            .context("failed to read target leg addr")?;
        let state = Arc::new(Mutex::new(SessionState::default()));
        let lease_secs = args.lease_secs.max(30);
        let started_at = unix_timestamp();
        let expires_at = started_at + lease_secs;
        let expires_at_instant = Instant::now() + Duration::from_secs(lease_secs);
        let request_id = request.request_id.clone();
        let network_id = request.network_id.clone();
        let target_pubkey = request.target_pubkey.clone();
        let requester_pubkey = message.sender_pubkey.clone();
        let requester_ingress_endpoint = SocketAddr::new(
            parse_advertise_ip(&args.advertise_host)?,
            requester_addr.port(),
        )
        .to_string();
        let target_ingress_endpoint = SocketAddr::new(
            parse_advertise_ip(&args.advertise_host)?,
            target_addr.port(),
        )
        .to_string();

        let response = RelayAllocationGranted {
            request_id: request_id.clone(),
            network_id: network_id.clone(),
            relay_pubkey: service_client.own_pubkey().to_string(),
            requester_ingress_endpoint: requester_ingress_endpoint.clone(),
            target_ingress_endpoint: target_ingress_endpoint.clone(),
            expires_at,
        };
        service_client
            .publish_to(
                ServicePayload::RelayAllocationGranted(response),
                &message.sender_pubkey,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to send allocation response to {}",
                    message.sender_pubkey
                )
            })?;

        {
            let mut stats = service_runtime_state.lock().await;
            stats.note_session_started(RelayOperatorSessionState {
                request_id: request_id.clone(),
                network_id,
                requester_pubkey,
                target_pubkey,
                requester_ingress_endpoint,
                target_ingress_endpoint,
                started_at,
                expires_at,
                bytes_from_requester: 0,
                bytes_from_target: 0,
            });
        }

        spawn_leg(
            requester_socket.clone(),
            target_socket.clone(),
            state.clone(),
            service_runtime_state.clone(),
            request_id.clone(),
            true,
            expires_at_instant,
        );
        spawn_leg(
            target_socket.clone(),
            requester_socket.clone(),
            state,
            service_runtime_state.clone(),
            request_id,
            false,
            expires_at_instant,
        );
    }

    service_client.disconnect().await;
    Ok(())
}

fn spawn_leg(
    rx_socket: Arc<UdpSocket>,
    tx_socket: Arc<UdpSocket>,
    state: Arc<Mutex<SessionState>>,
    service_runtime_state: Arc<Mutex<ServiceRuntimeState>>,
    request_id: String,
    requester_leg: bool,
    expires_at: Instant,
) {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 65_535];
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(expires_at) => return,
                recv = rx_socket.recv_from(&mut buffer) => {
                    let Ok((len, src)) = recv else {
                        return;
                    };
                    let maybe_dest = {
                        let mut state = state.lock().await;
                        if requester_leg {
                            match state.requester.bound_addr {
                                Some(bound) if bound != src => None,
                                Some(_) => state.target.bound_addr,
                                None => {
                                    state.requester.bound_addr = Some(src);
                                    state.target.bound_addr
                                }
                            }
                        } else {
                            match state.target.bound_addr {
                                Some(bound) if bound != src => None,
                                Some(_) => state.requester.bound_addr,
                                None => {
                                    state.target.bound_addr = Some(src);
                                    state.requester.bound_addr
                                }
                            }
                        }
                    };
                    if let Some(dest) = maybe_dest {
                        if let Ok(sent) = tx_socket.send_to(&buffer[..len], dest).await {
                            let mut stats = service_runtime_state.lock().await;
                            stats.note_forwarded_bytes(&request_id, requester_leg, sent as u64);
                        }
                    }
                }
            }
        }
    });
}

fn spawn_state_writer(path: PathBuf, service_runtime_state: Arc<Mutex<ServiceRuntimeState>>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(STATE_WRITE_INTERVAL_SECS));
        loop {
            interval.tick().await;
            let snapshot = {
                let mut stats = service_runtime_state.lock().await;
                stats.snapshot(unix_timestamp())
            };
            if let Err(error) = write_state_file(&path, &snapshot) {
                eprintln!(
                    "relay-service: failed to write state file {}: {error}",
                    path.display()
                );
            }
        }
    });
}

fn default_state_file_path() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join("nvpn").join(DEFAULT_STATE_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_FILE_NAME))
}

fn write_state_file(path: &Path, state: &ServiceOperatorState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_vec_pretty(state).context("failed to serialize relay state")?;
    write_runtime_file_atomically(path, &raw)?;
    set_private_state_file_permissions(path)?;
    Ok(())
}

fn write_runtime_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("runtime file has no parent: {}", path.display()))?;
    let temp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("relay-state"),
        std::process::id(),
        unix_timestamp()
    ));
    fs::write(&temp_path, contents)
        .with_context(|| format!("failed to write temp runtime file {}", temp_path.display()))?;
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to replace {} with {}",
            path.display(),
            temp_path.display()
        )
    })?;
    Ok(())
}

fn set_private_state_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!(
                "failed to set relay state file permissions on {}",
                path.display()
            )
        })?;
    }

    #[cfg(not(unix))]
    let _ = path;

    Ok(())
}

fn parse_advertise_ip(value: &str) -> Result<IpAddr> {
    value
        .parse::<IpAddr>()
        .with_context(|| format!("invalid advertise host '{value}', expected an IP address"))
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::env;
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{NatAssistRuntimeState, RelayRuntimeState, unix_timestamp};
    use nostr_vpn_core::relay::{
        NatAssistOperatorState, RelayOperatorSessionState, RelayOperatorState, ServiceOperatorState,
    };

    fn unique_state_path() -> PathBuf {
        env::temp_dir().join(format!(
            "nvpn-relay-state-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn relay_runtime_state_tracks_forwarded_bytes_and_unique_peers() {
        let mut state = RelayRuntimeState {
            relay_pubkey: "relay".to_string(),
            advertised_endpoint: "198.51.100.9:0".to_string(),
            ..RelayRuntimeState::default()
        };
        let now = unix_timestamp();
        state.note_session_started(RelayOperatorSessionState {
            request_id: "req-1".to_string(),
            network_id: "mesh-1".to_string(),
            requester_pubkey: "requester-a".to_string(),
            target_pubkey: "target-b".to_string(),
            requester_ingress_endpoint: "198.51.100.9:40001".to_string(),
            target_ingress_endpoint: "198.51.100.9:40002".to_string(),
            started_at: now.saturating_sub(2),
            expires_at: now + 60,
            bytes_from_requester: 0,
            bytes_from_target: 0,
        });
        state.note_forwarded_bytes("req-1", true, 512);
        state.note_forwarded_bytes("req-1", false, 256);
        let snapshot = state.snapshot(now);

        assert_eq!(snapshot.total_sessions_served, 1);
        assert_eq!(snapshot.total_forwarded_bytes, 768);
        assert_eq!(snapshot.unique_peer_count, 2);
        assert_eq!(snapshot.active_sessions.len(), 1);
        assert_eq!(snapshot.active_sessions[0].bytes_from_requester, 512);
        assert_eq!(snapshot.active_sessions[0].bytes_from_target, 256);
    }

    #[test]
    fn relay_runtime_state_prunes_expired_sessions_from_active_snapshot() {
        let now = unix_timestamp();
        let mut state = RelayRuntimeState {
            relay_pubkey: "relay".to_string(),
            advertised_endpoint: "198.51.100.9:0".to_string(),
            active_sessions: HashMap::from([(
                "expired".to_string(),
                RelayOperatorSessionState {
                    request_id: "expired".to_string(),
                    network_id: "mesh-1".to_string(),
                    requester_pubkey: "requester-a".to_string(),
                    target_pubkey: "target-b".to_string(),
                    requester_ingress_endpoint: "198.51.100.9:40001".to_string(),
                    target_ingress_endpoint: "198.51.100.9:40002".to_string(),
                    started_at: now.saturating_sub(10),
                    expires_at: now.saturating_sub(1),
                    bytes_from_requester: 100,
                    bytes_from_target: 50,
                },
            )]),
            ..RelayRuntimeState::default()
        };

        let snapshot = state.snapshot(now);
        assert!(snapshot.active_sessions.is_empty());
    }

    #[test]
    fn runtime_state_seeded_from_snapshot_keeps_cumulative_totals() {
        let now = unix_timestamp();
        let snapshot = RelayOperatorState {
            updated_at: now.saturating_sub(10),
            relay_pubkey: "old-relay".to_string(),
            advertised_endpoint: "198.51.100.9:0".to_string(),
            total_sessions_served: 4,
            total_forwarded_bytes: 8_192,
            current_forward_bps: 321,
            unique_peer_count: 3,
            known_peer_pubkeys: vec![
                "requester-a".to_string(),
                "target-b".to_string(),
                "target-c".to_string(),
            ],
            active_sessions: vec![RelayOperatorSessionState {
                request_id: "stale".to_string(),
                network_id: "mesh-1".to_string(),
                requester_pubkey: "requester-a".to_string(),
                target_pubkey: "target-b".to_string(),
                requester_ingress_endpoint: "198.51.100.9:40001".to_string(),
                target_ingress_endpoint: "198.51.100.9:40002".to_string(),
                started_at: now.saturating_sub(5),
                expires_at: now + 60,
                bytes_from_requester: 3_000,
                bytes_from_target: 2_000,
            }],
        };

        let state = super::relay_runtime_state_from_snapshot(
            snapshot,
            "relay-now".to_string(),
            "203.0.113.7:0".to_string(),
            now,
        );

        assert_eq!(state.relay_pubkey, "relay-now");
        assert_eq!(state.advertised_endpoint, "203.0.113.7:0");
        assert_eq!(state.total_sessions_served, 4);
        assert_eq!(state.total_forwarded_bytes, 8_192);
        assert_eq!(state.last_rate_sample_bytes, 8_192);
        assert_eq!(state.current_forward_bps, 0);
        assert_eq!(state.known_peer_pubkeys.len(), 3);
        assert!(state.active_sessions.is_empty());
    }

    #[test]
    fn nat_assist_runtime_state_tracks_requests_and_unique_clients() {
        let mut state = NatAssistRuntimeState {
            advertised_endpoint: "198.51.100.9:3478".to_string(),
            ..NatAssistRuntimeState::default()
        };
        let now = unix_timestamp();
        state.note_discovery_request(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(198, 51, 100, 10),
            50000,
        )));
        state.note_punch_request(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(198, 51, 100, 10),
            50001,
        )));
        state.note_discovery_request(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(198, 51, 100, 11),
            50002,
        )));

        let snapshot = state.snapshot(now);

        assert_eq!(snapshot.total_discovery_requests, 2);
        assert_eq!(snapshot.total_punch_requests, 1);
        assert_eq!(snapshot.unique_client_count, 2);
        assert_eq!(snapshot.advertised_endpoint, "198.51.100.9:3478");
    }

    #[test]
    fn load_runtime_state_upgrades_legacy_relay_snapshot() {
        let now = unix_timestamp();
        let path = unique_state_path();
        let legacy = RelayOperatorState {
            updated_at: now.saturating_sub(10),
            relay_pubkey: "old-relay".to_string(),
            advertised_endpoint: "198.51.100.9:0".to_string(),
            total_sessions_served: 4,
            total_forwarded_bytes: 8_192,
            current_forward_bps: 0,
            unique_peer_count: 2,
            known_peer_pubkeys: vec!["requester-a".to_string(), "target-b".to_string()],
            active_sessions: Vec::new(),
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&legacy).expect("serialize legacy state"),
        )
        .expect("write state");

        let state = super::load_runtime_state(
            &path,
            "relay-now".to_string(),
            Some("203.0.113.7:0".to_string()),
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 3478).to_string()),
        );

        assert_eq!(state.operator_pubkey, "relay-now");
        assert_eq!(
            state.relay.as_ref().expect("relay").total_sessions_served,
            4
        );
        assert_eq!(
            state.relay.as_ref().expect("relay").advertised_endpoint,
            "203.0.113.7:0"
        );
        assert_eq!(
            state
                .nat_assist
                .as_ref()
                .expect("nat assist")
                .advertised_endpoint,
            "203.0.113.7:3478"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn load_runtime_state_restores_service_snapshot() {
        let now = unix_timestamp();
        let path = unique_state_path();
        let snapshot = ServiceOperatorState {
            updated_at: now.saturating_sub(10),
            operator_pubkey: "service".to_string(),
            relay: Some(RelayOperatorState {
                updated_at: now.saturating_sub(10),
                relay_pubkey: "service".to_string(),
                advertised_endpoint: "198.51.100.9:0".to_string(),
                total_sessions_served: 4,
                total_forwarded_bytes: 8_192,
                current_forward_bps: 0,
                unique_peer_count: 2,
                known_peer_pubkeys: vec!["requester-a".to_string(), "target-b".to_string()],
                active_sessions: Vec::new(),
            }),
            nat_assist: Some(NatAssistOperatorState {
                updated_at: now.saturating_sub(10),
                advertised_endpoint: "198.51.100.9:3478".to_string(),
                total_discovery_requests: 7,
                total_punch_requests: 3,
                current_request_bps: 0,
                unique_client_count: 2,
            }),
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&snapshot).expect("serialize service state"),
        )
        .expect("write state");

        let state = super::load_runtime_state(
            &path,
            "service-now".to_string(),
            Some("203.0.113.7:0".to_string()),
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 3478).to_string()),
        );

        assert_eq!(state.operator_pubkey, "service-now");
        assert_eq!(
            state.relay.as_ref().expect("relay").total_sessions_served,
            4
        );
        assert_eq!(
            state
                .nat_assist
                .as_ref()
                .expect("nat assist")
                .total_discovery_requests,
            7
        );
        assert_eq!(
            state
                .nat_assist
                .as_ref()
                .expect("nat assist")
                .total_punch_requests,
            3
        );
        let _ = fs::remove_file(path);
    }
}
