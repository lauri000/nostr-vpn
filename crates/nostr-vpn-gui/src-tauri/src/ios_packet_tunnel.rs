use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::os::raw::c_uchar;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use nostr_vpn_core::config::{
    AppConfig, DEFAULT_RELAYS, maybe_autoconfigure_node, normalize_advertised_route,
};
use nostr_vpn_core::control::{PeerAnnouncement, select_peer_endpoint};
use nostr_vpn_core::paths::PeerPathBook;
use nostr_vpn_core::presence::PeerPresenceBook;
use nostr_vpn_core::signaling::{NostrSignalingClient, SignalPayload, SignalingNetwork};
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::mobile_wg::{MobileWireGuardRuntime, WireGuardPeerConfig};
use crate::{DaemonPeerState, DaemonRuntimeState, PEER_ONLINE_GRACE_SECS};

const IOS_ANNOUNCE_INTERVAL_SECS: u64 = 5;
const IOS_PUBLISH_TIMEOUT_SECS: u64 = 3;
const IOS_SIGNAL_STALE_AFTER_SECS: u64 = 45;
const IOS_TIMER_INTERVAL_MILLIS: u64 = 250;
const IOS_SESSION_STATUS_WAITING: &str = "Waiting for participants";
const IOS_TUN_MTU: u16 = 1_280;

type SettingsCallback = extern "C" fn(*const c_char, usize);
type PacketCallback = extern "C" fn(*const c_uchar, usize, usize);

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelStatusSnapshot {
    active: bool,
    error: Option<String>,
    state_json: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkSettingsPayload {
    local_addresses: Vec<String>,
    routes: Vec<String>,
    dns_servers: Vec<String>,
    search_domains: Vec<String>,
    mtu: u16,
}

#[derive(Debug, Clone)]
struct TunnelPeer {
    participant: String,
    pubkey_b64: String,
    endpoint: SocketAddr,
    allowed_ips: Vec<String>,
}

#[derive(Debug, Clone)]
struct PlannedTunnelPeer {
    participant: String,
    endpoint: String,
    peer: TunnelPeer,
}

#[derive(Clone, Copy)]
struct IosTunnelCallbacks {
    context: usize,
    settings_callback: SettingsCallback,
    packet_callback: PacketCallback,
}

struct IosTunnelHandle {
    runtime: tokio::runtime::Runtime,
    stop_tx: watch::Sender<bool>,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct IosTunnelController {
    active: Option<IosTunnelHandle>,
    snapshot: Arc<Mutex<TunnelStatusSnapshot>>,
}

struct ReconcileContext<'a> {
    own_pubkey: Option<&'a str>,
    recipients: &'a [String],
    listen_port: u16,
}

static TUNNEL_CONTROLLER: OnceLock<Mutex<IosTunnelController>> = OnceLock::new();

fn controller() -> &'static Mutex<IosTunnelController> {
    TUNNEL_CONTROLLER.get_or_init(|| Mutex::new(IosTunnelController::default()))
}

impl IosTunnelCallbacks {
    fn update_settings(&self, payload: &NetworkSettingsPayload) -> Result<()> {
        let json =
            serde_json::to_string(payload).context("failed to serialize network settings")?;
        let json = CString::new(json).context("network settings payload contained nul")?;
        (self.settings_callback)(json.as_ptr(), self.context);
        Ok(())
    }

    fn write_tunnel_packets(&self, packets: &[Vec<u8>]) {
        for packet in packets {
            (self.packet_callback)(packet.as_ptr(), packet.len(), self.context);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nvpn_ios_extension_start(
    config_json: *const c_char,
    context: usize,
    settings_callback: SettingsCallback,
    packet_callback: PacketCallback,
) -> bool {
    let result = start_tunnel(
        config_json,
        IosTunnelCallbacks {
            context,
            settings_callback,
            packet_callback,
        },
    );

    if let Err(error) = result {
        set_snapshot(
            false,
            Some(format!("failed to start iOS packet tunnel: {error}")),
            None,
        );
        eprintln!("ios-packet-tunnel: start failed: {error:#}");
        return false;
    }

    true
}

#[unsafe(no_mangle)]
pub extern "C" fn nvpn_ios_extension_push_packet(packet: *const c_uchar, length: usize) {
    if packet.is_null() || length == 0 {
        return;
    }

    let bytes = unsafe { std::slice::from_raw_parts(packet, length) }.to_vec();

    let Ok(guard) = controller().lock() else {
        return;
    };
    let Some(handle) = guard.active.as_ref() else {
        return;
    };
    let _ = handle.packet_tx.send(bytes);
}

#[unsafe(no_mangle)]
pub extern "C" fn nvpn_ios_extension_stop() {
    let handle = {
        let Ok(mut guard) = controller().lock() else {
            return;
        };
        guard.active.take()
    };

    if let Some(handle) = handle {
        let _ = handle.stop_tx.send(true);
        let _ = handle.runtime.block_on(handle.task);
    }

    set_snapshot(
        false,
        None,
        Some(DaemonRuntimeState {
            session_active: false,
            relay_connected: false,
            session_status: "Disconnected".to_string(),
            ..DaemonRuntimeState::default()
        }),
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn nvpn_ios_extension_status_json() -> *mut c_char {
    let snapshot = controller()
        .lock()
        .ok()
        .and_then(|guard| guard.snapshot.lock().ok().map(|snapshot| snapshot.clone()))
        .unwrap_or_default();

    CString::new(serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string()))
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "C" fn nvpn_ios_extension_free_string(value: *mut c_char) {
    if value.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(value);
    }
}

fn start_tunnel(config_json: *const c_char, callbacks: IosTunnelCallbacks) -> Result<()> {
    let config_json = unsafe {
        if config_json.is_null() {
            return Err(anyhow!("missing config JSON"));
        }
        CStr::from_ptr(config_json)
            .to_str()
            .context("config JSON was not valid UTF-8")?
            .to_string()
    };

    let mut config = serde_json::from_str::<AppConfig>(&config_json)
        .context("failed to parse iOS packet tunnel config")?;
    config.ensure_defaults();
    maybe_autoconfigure_node(&mut config);

    nvpn_ios_extension_stop();

    let snapshot = Arc::new(Mutex::new(TunnelStatusSnapshot::default()));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build iOS packet tunnel runtime")?;
    let (stop_tx, stop_rx) = watch::channel(false);
    let (packet_tx, packet_rx) = mpsc::unbounded_channel();

    set_snapshot(
        true,
        None,
        Some(DaemonRuntimeState {
            session_active: true,
            relay_connected: false,
            session_status: "Connecting…".to_string(),
            ..DaemonRuntimeState::default()
        }),
    );

    let snapshot_for_task = snapshot.clone();
    let task = runtime.spawn(async move {
        if let Err(error) = run_ios_packet_tunnel(
            config,
            snapshot_for_task.clone(),
            stop_rx,
            packet_rx,
            callbacks,
        )
        .await
        {
            eprintln!("ios-packet-tunnel: runtime failed: {error:#}");
            set_snapshot(
                false,
                Some(format!("Packet tunnel failed: {error}")),
                Some(DaemonRuntimeState {
                    session_active: false,
                    relay_connected: false,
                    session_status: format!("Packet tunnel failed: {error}"),
                    ..DaemonRuntimeState::default()
                }),
            );
        }
    });

    let mut guard = controller()
        .lock()
        .map_err(|_| anyhow!("packet tunnel controller lock poisoned"))?;
    guard.snapshot = snapshot.clone();
    guard.active = Some(IosTunnelHandle {
        runtime,
        stop_tx,
        packet_tx,
        task,
    });

    Ok(())
}

async fn run_ios_packet_tunnel(
    config: AppConfig,
    snapshot: Arc<Mutex<TunnelStatusSnapshot>>,
    mut stop_rx: watch::Receiver<bool>,
    mut packet_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    callbacks: IosTunnelCallbacks,
) -> Result<()> {
    let expected_peers = expected_peer_count(&config);
    let own_pubkey = config.own_nostr_pubkey_hex().ok();
    let relays = resolve_relays(&config);
    let recipients = configured_recipients(&config, own_pubkey.as_deref());

    callbacks.update_settings(&NetworkSettingsPayload {
        local_addresses: vec![local_interface_address_for_tunnel(&config.node.tunnel_ip)],
        routes: Vec::new(),
        dns_servers: Vec::new(),
        search_domains: Vec::new(),
        mtu: IOS_TUN_MTU,
    })?;

    let bind_socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, config.node.listen_port))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)))
        .context("failed to bind iOS WireGuard UDP socket")?;
    bind_socket
        .set_nonblocking(true)
        .context("failed to set iOS WireGuard UDP socket nonblocking")?;
    let listen_port = bind_socket
        .local_addr()
        .context("failed to read iOS WireGuard UDP socket address")?
        .port();
    let udp = tokio::net::UdpSocket::from_std(bind_socket)
        .context("failed to create async UDP socket")?;

    let client = NostrSignalingClient::from_secret_key_with_networks(
        &config.nostr.secret_key,
        signaling_networks_for_app(&config),
    )?;
    client
        .connect(&relays)
        .await
        .context("failed to connect signaling client")?;

    let mut presence = PeerPresenceBook::default();
    let mut path_book = PeerPathBook::default();
    let mut current_runtime: Option<MobileWireGuardRuntime> = None;
    let mut current_fingerprint: Option<String> = None;
    let mut current_route_targets = Vec::<String>::new();

    publish_hello_best_effort(&client).await;
    publish_private_announce_best_effort(&client, &config, listen_port, &recipients).await;
    update_snapshot(
        &snapshot,
        true,
        None,
        Some(build_runtime_state(
            &config,
            expected_peers,
            true,
            current_runtime.as_ref(),
            own_pubkey.as_deref(),
            &presence,
        )),
    );

    let mut announce_interval =
        tokio::time::interval(Duration::from_secs(IOS_ANNOUNCE_INTERVAL_SECS));
    let mut status_interval = tokio::time::interval(Duration::from_secs(1));
    let mut wireguard_timer =
        tokio::time::interval(Duration::from_millis(IOS_TIMER_INTERVAL_MILLIS));
    let mut udp_buf = vec![0_u8; 65_535];

    loop {
        tokio::select! {
            changed = stop_rx.changed() => {
                if changed.is_ok() && *stop_rx.borrow() {
                    break;
                }
            }
            packet = packet_rx.recv() => {
                let Some(packet) = packet else {
                    break;
                };
                if let Some(runtime) = current_runtime.as_mut() {
                    let outgoing = runtime
                        .queue_tunnel_packet(&packet)
                        .context("failed to queue tunnel packet")?;
                    send_outgoing_datagrams(&udp, outgoing).await?;
                    update_snapshot(
                        &snapshot,
                        true,
                        None,
                        Some(build_runtime_state(
                            &config,
                            expected_peers,
                            true,
                            current_runtime.as_ref(),
                            own_pubkey.as_deref(),
                            &presence,
                        )),
                    );
                }
            }
            envelope = client.recv() => {
                let Some(envelope) = envelope else {
                    return Err(anyhow!("signaling client closed"));
                };
                presence.apply_signal(
                    envelope.sender_pubkey,
                    envelope.payload,
                    unix_timestamp(),
                );
                reconcile_runtime(
                    &udp,
                    &client,
                    &config,
                    ReconcileContext {
                        own_pubkey: own_pubkey.as_deref(),
                        recipients: &recipients,
                        listen_port,
                    },
                    &mut presence,
                    &mut path_book,
                    &mut current_runtime,
                    &mut current_fingerprint,
                    &mut current_route_targets,
                    callbacks,
                )
                .await?;
                update_snapshot(
                    &snapshot,
                    true,
                    None,
                    Some(build_runtime_state(
                        &config,
                        expected_peers,
                        true,
                        current_runtime.as_ref(),
                        own_pubkey.as_deref(),
                        &presence,
                    )),
                );
            }
            recv = udp.recv_from(&mut udp_buf) => {
                let (read, source) = recv.context("failed to receive UDP datagram")?;
                if let Some(runtime) = current_runtime.as_mut() {
                    let processed = runtime
                        .receive_datagram(source, &udp_buf[..read])
                        .context("failed to process WireGuard datagram")?;
                    callbacks.write_tunnel_packets(&processed.tunnel_packets);
                    send_outgoing_datagrams(&udp, processed.outgoing).await?;
                    update_snapshot(
                        &snapshot,
                        true,
                        None,
                        Some(build_runtime_state(
                            &config,
                            expected_peers,
                            true,
                            current_runtime.as_ref(),
                            own_pubkey.as_deref(),
                            &presence,
                        )),
                    );
                }
            }
            _ = wireguard_timer.tick() => {
                if let Some(runtime) = current_runtime.as_mut() {
                    let processed = runtime.tick_timers();
                    callbacks.write_tunnel_packets(&processed.tunnel_packets);
                    send_outgoing_datagrams(&udp, processed.outgoing).await?;
                    update_snapshot(
                        &snapshot,
                        true,
                        None,
                        Some(build_runtime_state(
                            &config,
                            expected_peers,
                            true,
                            current_runtime.as_ref(),
                            own_pubkey.as_deref(),
                            &presence,
                        )),
                    );
                }
            }
            _ = announce_interval.tick() => {
                publish_hello_best_effort(&client).await;
                publish_private_announce_best_effort(&client, &config, listen_port, &recipients).await;
            }
            _ = status_interval.tick() => {
                let now = unix_timestamp();
                presence.prune_stale(now, IOS_SIGNAL_STALE_AFTER_SECS);
                note_successful_runtime_paths(current_runtime.as_ref(), &mut path_book, now);
                update_snapshot(
                    &snapshot,
                    true,
                    None,
                    Some(build_runtime_state(
                        &config,
                        expected_peers,
                        true,
                        current_runtime.as_ref(),
                        own_pubkey.as_deref(),
                        &presence,
                    )),
                );
            }
        }
    }

    let _ = client
        .publish_to(
            SignalPayload::Disconnect {
                node_id: config.node.id.clone(),
            },
            &recipients,
        )
        .await;
    client.disconnect().await;

    update_snapshot(
        &snapshot,
        false,
        None,
        Some(DaemonRuntimeState {
            session_active: false,
            relay_connected: false,
            session_status: "Disconnected".to_string(),
            ..DaemonRuntimeState::default()
        }),
    );

    Ok(())
}

async fn reconcile_runtime(
    udp: &tokio::net::UdpSocket,
    client: &NostrSignalingClient,
    config: &AppConfig,
    context: ReconcileContext<'_>,
    presence: &mut PeerPresenceBook,
    path_book: &mut PeerPathBook,
    current_runtime: &mut Option<MobileWireGuardRuntime>,
    current_fingerprint: &mut Option<String>,
    current_route_targets: &mut Vec<String>,
    callbacks: IosTunnelCallbacks,
) -> Result<()> {
    let now = unix_timestamp();
    let own_endpoint = local_signal_endpoint(config, context.listen_port);
    let planned = planned_tunnel_peers(
        config,
        context.own_pubkey,
        presence.known(),
        path_book,
        Some(&own_endpoint),
        now,
    )?;

    for peer in &planned {
        path_book.note_selected(&peer.participant, &peer.endpoint, now);
    }

    let local_addresses = vec![local_interface_address_for_tunnel(&config.node.tunnel_ip)];
    let route_targets = route_targets_for_tunnel_peers(&planned);
    if &route_targets != current_route_targets {
        callbacks.update_settings(&NetworkSettingsPayload {
            local_addresses,
            routes: route_targets.clone(),
            dns_servers: Vec::new(),
            search_domains: Vec::new(),
            mtu: IOS_TUN_MTU,
        })?;
        *current_route_targets = route_targets;
    }

    if planned.is_empty() {
        *current_runtime = None;
        *current_fingerprint = None;
        return Ok(());
    }

    let fingerprint = tunnel_fingerprint(config, context.listen_port, &planned);
    if current_fingerprint.as_deref() == Some(fingerprint.as_str()) {
        return Ok(());
    }

    let peer_configs = planned
        .iter()
        .map(|planned| WireGuardPeerConfig {
            participant_pubkey: planned.participant.clone(),
            public_key: planned.peer.pubkey_b64.clone(),
            endpoint: planned.peer.endpoint,
            allowed_ips: planned.peer.allowed_ips.clone(),
        })
        .collect::<Vec<_>>();
    let mut runtime = MobileWireGuardRuntime::new(&config.node.private_key, peer_configs)
        .context("failed to initialize iOS WireGuard runtime")?;

    send_outgoing_datagrams(udp, runtime.initiate_handshakes()).await?;
    *current_runtime = Some(runtime);
    *current_fingerprint = Some(fingerprint);
    publish_private_announce_best_effort(client, config, context.listen_port, context.recipients)
        .await;

    Ok(())
}

fn update_snapshot(
    snapshot: &Arc<Mutex<TunnelStatusSnapshot>>,
    active: bool,
    error: Option<String>,
    state: Option<DaemonRuntimeState>,
) {
    let state_json = state
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .ok()
        .flatten();

    apply_snapshot(snapshot, active, error.clone(), state_json.clone());

    if let Ok(guard) = controller().lock() {
        apply_snapshot(&guard.snapshot, active, error, state_json);
    }
}

fn set_snapshot(active: bool, error: Option<String>, state: Option<DaemonRuntimeState>) {
    if let Ok(guard) = controller().lock() {
        let state_json = state
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .ok()
            .flatten();
        apply_snapshot(&guard.snapshot, active, error, state_json);
    }
}

fn apply_snapshot(
    snapshot: &Arc<Mutex<TunnelStatusSnapshot>>,
    active: bool,
    error: Option<String>,
    state_json: Option<String>,
) {
    if let Ok(mut guard) = snapshot.lock() {
        guard.active = active;
        guard.error = error;
        guard.state_json = state_json;
    }
}

async fn send_outgoing_datagrams(
    udp: &tokio::net::UdpSocket,
    datagrams: Vec<crate::mobile_wg::OutgoingDatagram>,
) -> Result<()> {
    for datagram in datagrams {
        udp.send_to(&datagram.payload, datagram.endpoint)
            .await
            .with_context(|| {
                format!("failed to send WireGuard datagram to {}", datagram.endpoint)
            })?;
    }
    Ok(())
}

fn build_runtime_state(
    config: &AppConfig,
    expected_peers: usize,
    relay_connected: bool,
    current_runtime: Option<&MobileWireGuardRuntime>,
    own_pubkey: Option<&str>,
    presence: &PeerPresenceBook,
) -> DaemonRuntimeState {
    let runtime_peer_map = current_runtime
        .map(|runtime| {
            runtime
                .peer_statuses()
                .into_iter()
                .map(|status| (status.participant_pubkey.clone(), status))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let peers = config
        .participant_pubkeys_hex()
        .into_iter()
        .filter(|participant| Some(participant.as_str()) != own_pubkey)
        .filter_map(|participant| {
            let announcement = presence.announcement_for(&participant)?;
            let runtime_status = runtime_peer_map.get(&participant);
            let last_handshake_at = runtime_status.and_then(|status| {
                status
                    .last_handshake_age
                    .and_then(|age| unix_timestamp().checked_sub(age.as_secs()))
            });
            let reachable = runtime_status
                .and_then(|status| status.last_handshake_age)
                .is_some_and(|age| age <= Duration::from_secs(PEER_ONLINE_GRACE_SECS));
            Some(DaemonPeerState {
                participant_pubkey: participant,
                node_id: announcement.node_id.clone(),
                tunnel_ip: announcement.tunnel_ip.clone(),
                endpoint: runtime_status
                    .map(|status| status.endpoint.to_string())
                    .unwrap_or_else(|| announcement.endpoint.clone()),
                public_key: announcement.public_key.clone(),
                advertised_routes: announcement.advertised_routes.clone(),
                presence_timestamp: announcement.timestamp,
                last_signal_seen_at: presence.last_seen_at(&announcement.node_id),
                reachable,
                last_handshake_at,
                error: if reachable {
                    None
                } else if runtime_status.is_some() {
                    Some("awaiting handshake".to_string())
                } else {
                    Some("no signal yet".to_string())
                },
            })
        })
        .collect::<Vec<_>>();

    let connected_peer_count = peers.iter().filter(|peer| peer.reachable).count();
    let mesh_ready = expected_peers > 0 && connected_peer_count >= expected_peers;

    DaemonRuntimeState {
        updated_at: unix_timestamp(),
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        session_active: true,
        relay_connected,
        session_status: if expected_peers == 0 {
            IOS_SESSION_STATUS_WAITING.to_string()
        } else if mesh_ready {
            "Connected".to_string()
        } else {
            format!("Connecting mesh ({connected_peer_count}/{expected_peers})")
        },
        expected_peer_count: expected_peers,
        connected_peer_count,
        mesh_ready,
        health: Vec::new(),
        network: Default::default(),
        port_mapping: Default::default(),
        peers,
    }
}

fn expected_peer_count(config: &AppConfig) -> usize {
    let participants = config.participant_pubkeys_hex();
    if participants.is_empty() {
        return 0;
    }

    let mut expected = participants.len();
    if let Ok(own_pubkey) = config.own_nostr_pubkey_hex()
        && participants
            .iter()
            .any(|participant| participant == &own_pubkey)
    {
        expected = expected.saturating_sub(1);
    }

    expected
}

fn resolve_relays(config: &AppConfig) -> Vec<String> {
    if !config.nostr.relays.is_empty() {
        return config.nostr.relays.clone();
    }

    DEFAULT_RELAYS
        .iter()
        .map(|relay| (*relay).to_string())
        .collect()
}

fn configured_recipients(config: &AppConfig, own_pubkey: Option<&str>) -> Vec<String> {
    config
        .participant_pubkeys_hex()
        .into_iter()
        .filter(|participant| Some(participant.as_str()) != own_pubkey)
        .collect()
}

async fn publish_private_announce_to_all(
    client: &NostrSignalingClient,
    config: &AppConfig,
    listen_port: u16,
    recipients: &[String],
) -> Result<()> {
    if recipients.is_empty() {
        return Ok(());
    }

    client
        .publish_to(
            SignalPayload::Announce(build_peer_announcement(config, listen_port)),
            recipients,
        )
        .await
        .context("failed to publish iOS private announce")?;
    Ok(())
}

async fn publish_private_announce_best_effort(
    client: &NostrSignalingClient,
    config: &AppConfig,
    listen_port: u16,
    recipients: &[String],
) {
    let _ = tokio::time::timeout(
        Duration::from_secs(IOS_PUBLISH_TIMEOUT_SECS),
        publish_private_announce_to_all(client, config, listen_port, recipients),
    )
    .await;
}

async fn publish_hello_best_effort(client: &NostrSignalingClient) {
    let _ = tokio::time::timeout(
        Duration::from_secs(IOS_PUBLISH_TIMEOUT_SECS),
        client.publish(SignalPayload::Hello),
    )
    .await;
}

fn build_peer_announcement(config: &AppConfig, listen_port: u16) -> PeerAnnouncement {
    let endpoint = local_signal_endpoint(config, listen_port);
    PeerAnnouncement {
        node_id: config.node.id.clone(),
        public_key: config.node.public_key.clone(),
        endpoint: endpoint.clone(),
        local_endpoint: Some(endpoint),
        public_endpoint: None,
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: config.node.tunnel_ip.clone(),
        advertised_routes: config.effective_advertised_routes(),
        timestamp: unix_timestamp(),
    }
}

fn planned_tunnel_peers(
    config: &AppConfig,
    own_pubkey: Option<&str>,
    peer_announcements: &HashMap<String, PeerAnnouncement>,
    path_book: &mut PeerPathBook,
    own_local_endpoint: Option<&str>,
    now: u64,
) -> Result<Vec<PlannedTunnelPeer>> {
    let configured_participants = config.participant_pubkeys_hex();
    let route_assignments = advertised_route_assignments(config, own_pubkey, peer_announcements);
    let configured_set = configured_participants
        .iter()
        .filter(|participant| Some(participant.as_str()) != own_pubkey)
        .cloned()
        .collect::<HashSet<_>>();
    path_book.retain_participants(&configured_set);

    let mut peers = Vec::new();
    for participant in configured_participants
        .iter()
        .filter(|participant| Some(participant.as_str()) != own_pubkey)
    {
        let Some(announcement) = peer_announcements.get(participant) else {
            continue;
        };
        path_book.refresh_from_announcement(participant.clone(), announcement, now);
        let selected_endpoint = path_book
            .select_endpoint(
                participant,
                announcement,
                own_local_endpoint,
                now,
                IOS_SIGNAL_STALE_AFTER_SECS,
            )
            .unwrap_or_else(|| select_peer_endpoint(announcement, own_local_endpoint));
        let endpoint: SocketAddr = selected_endpoint
            .parse()
            .with_context(|| format!("invalid peer endpoint {selected_endpoint}"))?;

        let mut allowed_ips = vec![format!("{}/32", strip_cidr(&announcement.tunnel_ip))];
        for route in route_assignments
            .get(participant)
            .into_iter()
            .flatten()
            .cloned()
        {
            if !allowed_ips.iter().any(|existing| existing == &route) {
                allowed_ips.push(route);
            }
        }

        peers.push(PlannedTunnelPeer {
            participant: participant.clone(),
            endpoint: selected_endpoint,
            peer: TunnelPeer {
                participant: participant.clone(),
                pubkey_b64: announcement.public_key.clone(),
                endpoint,
                allowed_ips,
            },
        });
    }

    peers.sort_by(|left, right| left.participant.cmp(&right.participant));
    Ok(peers)
}

fn advertised_route_assignments(
    config: &AppConfig,
    own_pubkey: Option<&str>,
    peer_announcements: &HashMap<String, PeerAnnouncement>,
) -> HashMap<String, Vec<String>> {
    let selected_exit_node = selected_exit_node_participant(config, own_pubkey, peer_announcements);
    let mut route_owner = HashMap::<String, String>::new();

    for participant in config
        .participant_pubkeys_hex()
        .iter()
        .filter(|participant| Some(participant.as_str()) != own_pubkey)
    {
        let Some(announcement) = peer_announcements.get(participant) else {
            continue;
        };

        for route in normalized_peer_ipv4_routes(announcement) {
            if is_exit_node_route(&route)
                && selected_exit_node.as_deref() != Some(participant.as_str())
            {
                continue;
            }
            route_owner
                .entry(route)
                .or_insert_with(|| participant.clone());
        }
    }

    let mut assignments = HashMap::<String, Vec<String>>::new();
    for (route, participant) in route_owner {
        assignments.entry(participant).or_default().push(route);
    }

    for routes in assignments.values_mut() {
        routes.sort();
        routes.dedup();
    }

    assignments
}

fn normalized_peer_ipv4_routes(announcement: &PeerAnnouncement) -> Vec<String> {
    let mut routes = Vec::new();
    let mut seen = HashSet::new();

    for route in &announcement.advertised_routes {
        let Some(route) = normalize_advertised_route(route) else {
            continue;
        };
        if strip_cidr(&route).parse::<Ipv4Addr>().is_err() {
            continue;
        }
        if seen.insert(route.clone()) {
            routes.push(route);
        }
    }

    routes
}

fn selected_exit_node_participant(
    config: &AppConfig,
    own_pubkey: Option<&str>,
    peer_announcements: &HashMap<String, PeerAnnouncement>,
) -> Option<String> {
    if config.exit_node.is_empty() || Some(config.exit_node.as_str()) == own_pubkey {
        return None;
    }

    let announcement = peer_announcements.get(&config.exit_node)?;
    normalized_peer_ipv4_routes(announcement)
        .iter()
        .any(|route| route == "0.0.0.0/0")
        .then(|| config.exit_node.clone())
}

fn is_exit_node_route(route: &str) -> bool {
    route == "0.0.0.0/0" || route == "::/0"
}

fn route_targets_for_tunnel_peers(peers: &[PlannedTunnelPeer]) -> Vec<String> {
    let mut route_targets = peers
        .iter()
        .flat_map(|peer| peer.peer.allowed_ips.iter().cloned())
        .collect::<Vec<_>>();
    route_targets.sort();
    route_targets.dedup();
    route_targets
}

fn tunnel_fingerprint(config: &AppConfig, listen_port: u16, peers: &[PlannedTunnelPeer]) -> String {
    let local_address = local_interface_address_for_tunnel(&config.node.tunnel_ip);
    let mut peer_entries = peers
        .iter()
        .map(|peer| {
            format!(
                "{}|{}|{}|{}",
                peer.peer.participant,
                peer.peer.pubkey_b64,
                peer.peer.endpoint,
                peer.peer.allowed_ips.join(",")
            )
        })
        .collect::<Vec<_>>();
    peer_entries.sort();

    format!(
        "{}|{}|{}|{}|{}",
        config.node.private_key,
        config.node.tunnel_ip,
        listen_port,
        local_address,
        peer_entries.join(";")
    )
}

fn local_interface_address_for_tunnel(tunnel_ip: &str) -> String {
    if tunnel_ip.contains('/') {
        tunnel_ip.to_string()
    } else {
        format!("{}/32", strip_cidr(tunnel_ip))
    }
}

fn local_signal_endpoint(config: &AppConfig, listen_port: u16) -> String {
    runtime_local_signal_endpoint(&config.node.endpoint, listen_port)
}

fn runtime_local_signal_endpoint(endpoint: &str, listen_port: u16) -> String {
    let value = endpoint.trim();
    if (value.is_empty() || matches!(value, "127.0.0.1:51820" | "127.0.0.1" | "0.0.0.0"))
        && let Some(ip) = detect_runtime_primary_ipv4()
    {
        return format!("{ip}:{listen_port}");
    }

    endpoint
        .parse::<SocketAddr>()
        .map(|mut parsed| {
            parsed.set_port(listen_port);
            parsed.to_string()
        })
        .unwrap_or_else(|_| endpoint.to_string())
}

fn detect_runtime_primary_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

fn note_successful_runtime_paths(
    current_runtime: Option<&MobileWireGuardRuntime>,
    path_book: &mut PeerPathBook,
    now: u64,
) {
    let Some(current_runtime) = current_runtime else {
        return;
    };

    for status in current_runtime.peer_statuses() {
        let Some(handshake_age) = status.last_handshake_age else {
            continue;
        };
        if handshake_age > Duration::from_secs(PEER_ONLINE_GRACE_SECS) {
            continue;
        }
        let success_at = now.saturating_sub(handshake_age.as_secs());
        path_book.note_success(
            status.participant_pubkey,
            &status.endpoint.to_string(),
            success_at,
        );
    }
}

fn strip_cidr(value: &str) -> &str {
    value.split('/').next().unwrap_or(value)
}

fn signaling_networks_for_app(app: &AppConfig) -> Vec<SignalingNetwork> {
    let networks = app
        .enabled_network_meshes()
        .into_iter()
        .map(|network| SignalingNetwork {
            network_id: network.network_id,
            participants: network.participants,
        })
        .collect::<Vec<_>>();

    if networks.is_empty() {
        return vec![SignalingNetwork {
            network_id: app.effective_network_id(),
            participants: app.participant_pubkeys_hex(),
        }];
    }

    networks
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
