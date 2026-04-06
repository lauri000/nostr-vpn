use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
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
    RelayAllocationGranted, RelayAllocationRejectReason, RelayAllocationRejected,
    RelayOperatorSessionState, RelayProbeGranted, RelayProbeRejected,
};
use nostr_vpn_core::service_signaling::{RelayServiceClient, ServicePayload};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::Instant;

#[path = "nvpn_udp_relay/port_allocator.rs"]
mod port_allocator;
#[path = "nvpn_udp_relay/relay_runtime.rs"]
mod relay_runtime;

use port_allocator::{RelayPortAllocator, bind_relay_leg_pair, bind_udp_socket, relay_port_range};
pub(crate) use relay_runtime::{
    RelayServiceLimits, ServiceRuntimeState, SessionForwardingState, default_state_file_path,
    load_runtime_state, write_state_file,
};

#[cfg(test)]
use port_allocator::RelayPortRange;

const DEFAULT_LEASE_SECS: u64 = 120;
const DEFAULT_PROBE_LEASE_SECS: u64 = 8;
const DEFAULT_PUBLISH_INTERVAL_SECS: u64 = 30;
const DEFAULT_NAT_ASSIST_PORT: u16 = 3478;
const DEFAULT_MAX_ACTIVE_RELAY_SESSIONS: usize = 64;
const DEFAULT_MAX_SESSIONS_PER_REQUESTER: usize = 8;
const DEFAULT_MAX_BYTES_PER_SESSION: u64 = 128 * 1024 * 1024;
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
    #[arg(long)]
    relay_port_range_start: Option<u16>,
    #[arg(long)]
    relay_port_range_end: Option<u16>,
    #[arg(long, default_value_t = DEFAULT_PUBLISH_INTERVAL_SECS)]
    publish_interval_secs: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_ACTIVE_RELAY_SESSIONS)]
    max_active_sessions: usize,
    #[arg(long, default_value_t = DEFAULT_MAX_SESSIONS_PER_REQUESTER)]
    max_sessions_per_requester: usize,
    #[arg(long, default_value_t = DEFAULT_MAX_BYTES_PER_SESSION)]
    max_bytes_per_session: u64,
    #[arg(long)]
    max_forward_bps: Option<u64>,
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
    forwarding: SessionForwardingState,
}

struct RelayLegTask {
    rx_socket: Arc<UdpSocket>,
    tx_socket: Arc<UdpSocket>,
    state: Arc<Mutex<SessionState>>,
    service_runtime_state: Arc<Mutex<ServiceRuntimeState>>,
    relay_limits: RelayServiceLimits,
    request_id: String,
    requester_leg: bool,
    expires_at: Instant,
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
) -> Result<()> {
    let bind_addr = SocketAddr::new(bind_ip, port);
    let socket = bind_udp_socket(bind_addr)
        .with_context(|| format!("failed to bind nat assist on {bind_addr}"))?;
    tokio::spawn(async move {
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
    Ok(())
}

async fn send_allocation_rejection(
    service_client: &RelayServiceClient,
    recipient_pubkey: &str,
    request_id: String,
    network_id: String,
    reason: RelayAllocationRejectReason,
    retry_after_secs: Option<u64>,
) -> Result<()> {
    service_client
        .publish_to(
            ServicePayload::RelayAllocationRejected(RelayAllocationRejected {
                request_id,
                network_id,
                relay_pubkey: service_client.own_pubkey().to_string(),
                reason,
                retry_after_secs,
            }),
            recipient_pubkey,
        )
        .await
}

async fn send_probe_rejection(
    service_client: &RelayServiceClient,
    recipient_pubkey: &str,
    request_id: String,
    reason: RelayAllocationRejectReason,
    retry_after_secs: Option<u64>,
) -> Result<()> {
    service_client
        .publish_to(
            ServicePayload::RelayProbeRejected(RelayProbeRejected {
                request_id,
                relay_pubkey: service_client.own_pubkey().to_string(),
                reason,
                retry_after_secs,
            }),
            recipient_pubkey,
        )
        .await
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
    let relay_port_range = relay_port_range(&args)?;
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
    let relay_limits = RelayServiceLimits {
        max_active_sessions: args.max_active_sessions.max(1),
        max_sessions_per_requester: args.max_sessions_per_requester.max(1),
        max_bytes_per_session: args.max_bytes_per_session.max(1),
        max_forward_bps: args.max_forward_bps.filter(|value| *value > 0),
    };
    let relay_port_allocator = relay_port_range
        .map(|range| Arc::new(std::sync::Mutex::new(RelayPortAllocator::new(range))));
    spawn_state_writer(state_file, service_runtime_state.clone());
    spawn_node_record_publisher(&args)?;
    if args.enable_nat_assist {
        spawn_nat_assist_listener(bind_ip, args.nat_assist_port, service_runtime_state.clone())?;
    }

    if args.disable_relay {
        std::future::pending::<()>().await;
        return Ok(());
    }

    loop {
        let Some(message) = service_client.recv().await else {
            break;
        };
        match message.payload {
            ServicePayload::RelayAllocationRequest(request) => {
                if let Some((reason, retry_after_secs)) = service_runtime_state
                    .lock()
                    .await
                    .relay
                    .as_mut()
                    .and_then(|relay| {
                        relay.allocation_rejection_for_requester(
                            &message.sender_pubkey,
                            unix_timestamp(),
                            &relay_limits,
                        )
                    })
                {
                    send_allocation_rejection(
                        &service_client,
                        &message.sender_pubkey,
                        request.request_id.clone(),
                        request.network_id.clone(),
                        reason,
                        retry_after_secs,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to send allocation rejection to {}",
                            message.sender_pubkey
                        )
                    })?;
                    continue;
                }

                let (
                    requester_socket,
                    target_socket,
                    requester_ingress_endpoint,
                    target_ingress_endpoint,
                ) = match bind_relay_leg_pair(
                    bind_ip,
                    &args.advertise_host,
                    relay_port_allocator.as_ref(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!(
                            "relay-service: failed to bind relay ports for allocation {}: {error}",
                            request.request_id
                        );
                        if let Err(rejection_error) = send_allocation_rejection(
                            &service_client,
                            &message.sender_pubkey,
                            request.request_id.clone(),
                            request.network_id.clone(),
                            RelayAllocationRejectReason::OverCapacity,
                            Some(30),
                        )
                        .await
                        {
                            eprintln!(
                                "relay-service: failed to send allocation rejection to {} after bind failure: {rejection_error}",
                                message.sender_pubkey
                            );
                        }
                        continue;
                    }
                };
                let state = Arc::new(Mutex::new(SessionState::default()));
                let lease_secs = args.lease_secs.max(30);
                let started_at = unix_timestamp();
                let expires_at = started_at + lease_secs;
                let expires_at_instant = Instant::now() + Duration::from_secs(lease_secs);
                let request_id = request.request_id.clone();
                let network_id = request.network_id.clone();
                let target_pubkey = request.target_pubkey.clone();
                let requester_pubkey = message.sender_pubkey.clone();

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

                spawn_leg(RelayLegTask {
                    rx_socket: requester_socket.clone(),
                    tx_socket: target_socket.clone(),
                    state: state.clone(),
                    service_runtime_state: service_runtime_state.clone(),
                    relay_limits,
                    request_id: request_id.clone(),
                    requester_leg: true,
                    expires_at: expires_at_instant,
                });
                spawn_leg(RelayLegTask {
                    rx_socket: target_socket.clone(),
                    tx_socket: requester_socket.clone(),
                    state,
                    service_runtime_state: service_runtime_state.clone(),
                    relay_limits,
                    request_id,
                    requester_leg: false,
                    expires_at: expires_at_instant,
                });
            }
            ServicePayload::RelayProbeRequest(request) => {
                if let Some((reason, retry_after_secs)) = service_runtime_state
                    .lock()
                    .await
                    .relay
                    .as_mut()
                    .and_then(|relay| {
                        relay.allocation_rejection_for_requester(
                            &message.sender_pubkey,
                            unix_timestamp(),
                            &relay_limits,
                        )
                    })
                {
                    send_probe_rejection(
                        &service_client,
                        &message.sender_pubkey,
                        request.request_id.clone(),
                        reason,
                        retry_after_secs,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to send probe rejection to {}",
                            message.sender_pubkey
                        )
                    })?;
                    continue;
                }

                let (
                    requester_socket,
                    target_socket,
                    requester_ingress_endpoint,
                    target_ingress_endpoint,
                ) = match bind_relay_leg_pair(
                    bind_ip,
                    &args.advertise_host,
                    relay_port_allocator.as_ref(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!(
                            "relay-service: failed to bind relay ports for probe {}: {error}",
                            request.request_id
                        );
                        if let Err(rejection_error) = send_probe_rejection(
                            &service_client,
                            &message.sender_pubkey,
                            request.request_id.clone(),
                            RelayAllocationRejectReason::OverCapacity,
                            Some(30),
                        )
                        .await
                        {
                            eprintln!(
                                "relay-service: failed to send probe rejection to {} after bind failure: {rejection_error}",
                                message.sender_pubkey
                            );
                        }
                        continue;
                    }
                };
                let state = Arc::new(Mutex::new(SessionState::default()));
                let lease_secs = DEFAULT_PROBE_LEASE_SECS;
                let started_at = unix_timestamp();
                let expires_at = started_at + lease_secs;
                let expires_at_instant = Instant::now() + Duration::from_secs(lease_secs);
                let request_id = request.request_id.clone();

                service_client
                    .publish_to(
                        ServicePayload::RelayProbeGranted(RelayProbeGranted {
                            request_id: request_id.clone(),
                            relay_pubkey: service_client.own_pubkey().to_string(),
                            requester_ingress_endpoint: requester_ingress_endpoint.clone(),
                            target_ingress_endpoint: target_ingress_endpoint.clone(),
                            expires_at,
                        }),
                        &message.sender_pubkey,
                    )
                    .await
                    .with_context(|| {
                        format!("failed to send probe grant to {}", message.sender_pubkey)
                    })?;

                {
                    let mut stats = service_runtime_state.lock().await;
                    stats.note_session_started(RelayOperatorSessionState {
                        request_id: request_id.clone(),
                        network_id: "__probe__".to_string(),
                        requester_pubkey: message.sender_pubkey.clone(),
                        target_pubkey: message.sender_pubkey.clone(),
                        requester_ingress_endpoint,
                        target_ingress_endpoint,
                        started_at,
                        expires_at,
                        bytes_from_requester: 0,
                        bytes_from_target: 0,
                    });
                }

                spawn_leg(RelayLegTask {
                    rx_socket: requester_socket.clone(),
                    tx_socket: target_socket.clone(),
                    state: state.clone(),
                    service_runtime_state: service_runtime_state.clone(),
                    relay_limits,
                    request_id: request_id.clone(),
                    requester_leg: true,
                    expires_at: expires_at_instant,
                });
                spawn_leg(RelayLegTask {
                    rx_socket: target_socket.clone(),
                    tx_socket: requester_socket.clone(),
                    state,
                    service_runtime_state: service_runtime_state.clone(),
                    relay_limits,
                    request_id,
                    requester_leg: false,
                    expires_at: expires_at_instant,
                });
            }
            ServicePayload::RelayAllocationGranted(_)
            | ServicePayload::RelayAllocationRejected(_)
            | ServicePayload::RelayProbeGranted(_)
            | ServicePayload::RelayProbeRejected(_) => {}
        }
    }

    service_client.disconnect().await;
    Ok(())
}

fn spawn_leg(task: RelayLegTask) {
    let RelayLegTask {
        rx_socket,
        tx_socket,
        state,
        service_runtime_state,
        relay_limits,
        request_id,
        requester_leg,
        expires_at,
    } = task;
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
                        if state
                            .forwarding
                            .allow_forward(&relay_limits, Instant::now(), len)
                            .is_err()
                        {
                            None
                        } else if requester_leg {
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
                    if let Some(dest) = maybe_dest
                        && let Ok(sent) = tx_socket.send_to(&buffer[..len], dest).await
                    {
                        let mut stats = service_runtime_state.lock().await;
                        stats.note_forwarded_bytes(&request_id, requester_leg, sent as u64);
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
#[path = "nvpn_udp_relay/tests.rs"]
mod tests;
