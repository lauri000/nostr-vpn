use std::collections::HashMap;
use std::env;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{
    DEFAULT_LEASE_SECS, DEFAULT_MAX_ACTIVE_RELAY_SESSIONS, DEFAULT_MAX_BYTES_PER_SESSION,
    DEFAULT_MAX_SESSIONS_PER_REQUESTER, DEFAULT_NAT_ASSIST_PORT, DEFAULT_PUBLISH_INTERVAL_SECS,
    NatAssistRuntimeState, RelayPortAllocator, RelayPortRange, RelayRuntimeState,
    RelayServiceLimits, SessionForwardingState, bind_relay_leg_pair, relay_port_range,
    unix_timestamp,
};
use nostr_vpn_core::relay::{
    NatAssistOperatorState, RelayAllocationRejectReason, RelayOperatorSessionState,
    RelayOperatorState, ServiceOperatorState,
};
use tokio::time::Instant;

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

fn find_reserved_udp_range(port_count: usize) -> (u16, Vec<std::net::UdpSocket>) {
    for start in 40_000_u16..60_000_u16 {
        let end = start.saturating_add(port_count as u16).saturating_sub(1);
        if end < start {
            break;
        }
        let mut reservations = Vec::with_capacity(port_count);
        let mut all_free = true;
        for port in start..=end {
            match std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
            {
                Ok(socket) => reservations.push(socket),
                Err(_) => {
                    all_free = false;
                    break;
                }
            }
        }
        if all_free {
            return (start, reservations);
        }
    }

    panic!("failed to reserve UDP test range");
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
fn relay_runtime_state_rejects_when_over_capacity() {
    let now = unix_timestamp();
    let mut state = RelayRuntimeState {
        relay_pubkey: "relay".to_string(),
        advertised_endpoint: "198.51.100.9:0".to_string(),
        active_sessions: HashMap::from([
            (
                "req-1".to_string(),
                RelayOperatorSessionState {
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
                },
            ),
            (
                "req-2".to_string(),
                RelayOperatorSessionState {
                    request_id: "req-2".to_string(),
                    network_id: "mesh-1".to_string(),
                    requester_pubkey: "requester-c".to_string(),
                    target_pubkey: "target-d".to_string(),
                    requester_ingress_endpoint: "198.51.100.9:40003".to_string(),
                    target_ingress_endpoint: "198.51.100.9:40004".to_string(),
                    started_at: now.saturating_sub(2),
                    expires_at: now + 60,
                    bytes_from_requester: 0,
                    bytes_from_target: 0,
                },
            ),
        ]),
        ..RelayRuntimeState::default()
    };

    let rejection = state.allocation_rejection_for_requester(
        "requester-z",
        now,
        &RelayServiceLimits {
            max_active_sessions: 2,
            max_sessions_per_requester: 4,
            max_bytes_per_session: 1_024,
            max_forward_bps: None,
        },
    );

    assert_eq!(
        rejection,
        Some((RelayAllocationRejectReason::OverCapacity, Some(30)))
    );
}

#[test]
fn relay_runtime_state_rejects_requester_when_session_cap_reached() {
    let now = unix_timestamp();
    let mut state = RelayRuntimeState {
        relay_pubkey: "relay".to_string(),
        advertised_endpoint: "198.51.100.9:0".to_string(),
        active_sessions: HashMap::from([(
            "req-1".to_string(),
            RelayOperatorSessionState {
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
            },
        )]),
        ..RelayRuntimeState::default()
    };

    let rejection = state.allocation_rejection_for_requester(
        "requester-a",
        now,
        &RelayServiceLimits {
            max_active_sessions: 8,
            max_sessions_per_requester: 1,
            max_bytes_per_session: 1_024,
            max_forward_bps: None,
        },
    );

    assert_eq!(
        rejection,
        Some((
            RelayAllocationRejectReason::TooManySessionsForRequester,
            Some(60)
        ))
    );
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
fn session_forwarding_state_enforces_byte_cap() {
    let mut state = SessionForwardingState::default();
    let limits = RelayServiceLimits {
        max_active_sessions: 8,
        max_sessions_per_requester: 2,
        max_bytes_per_session: 512,
        max_forward_bps: None,
    };
    let now = Instant::now();

    assert!(state.allow_forward(&limits, now, 256).is_ok());
    assert!(state.allow_forward(&limits, now, 256).is_ok());
    assert_eq!(
        state.allow_forward(&limits, now, 1),
        Err(RelayAllocationRejectReason::ByteLimitExceeded)
    );
}

#[test]
fn session_forwarding_state_enforces_rate_limit() {
    let mut state = SessionForwardingState::default();
    let limits = RelayServiceLimits {
        max_active_sessions: 8,
        max_sessions_per_requester: 2,
        max_bytes_per_session: 4_096,
        max_forward_bps: Some(512),
    };
    let now = Instant::now();

    assert!(state.allow_forward(&limits, now, 512).is_ok());
    assert_eq!(
        state.allow_forward(&limits, now, 1),
        Err(RelayAllocationRejectReason::RateLimited)
    );
    assert!(
        state
            .allow_forward(&limits, now + Duration::from_secs(1), 256)
            .is_ok()
    );
}

#[test]
fn relay_port_range_requires_both_bounds() {
    let args = super::Args {
        secret_key: "00".repeat(32),
        relays: vec!["wss://temp.iris.to".to_string()],
        bind_ip: "0.0.0.0".to_string(),
        advertise_host: "203.0.113.7".to_string(),
        disable_relay: false,
        enable_nat_assist: false,
        nat_assist_port: DEFAULT_NAT_ASSIST_PORT,
        lease_secs: DEFAULT_LEASE_SECS,
        relay_port_range_start: Some(12_000),
        relay_port_range_end: None,
        publish_interval_secs: DEFAULT_PUBLISH_INTERVAL_SECS,
        max_active_sessions: DEFAULT_MAX_ACTIVE_RELAY_SESSIONS,
        max_sessions_per_requester: DEFAULT_MAX_SESSIONS_PER_REQUESTER,
        max_bytes_per_session: DEFAULT_MAX_BYTES_PER_SESSION,
        max_forward_bps: None,
        price_hint_msats: None,
        state_file: None,
    };

    let error = relay_port_range(&args).expect_err("missing range end should fail");
    assert!(error.to_string().contains(
        "both --relay-port-range-start and --relay-port-range-end are required together"
    ));
}

#[test]
fn relay_port_range_rejects_nat_assist_overlap() {
    let args = super::Args {
        secret_key: "00".repeat(32),
        relays: vec!["wss://temp.iris.to".to_string()],
        bind_ip: "0.0.0.0".to_string(),
        advertise_host: "203.0.113.7".to_string(),
        disable_relay: false,
        enable_nat_assist: true,
        nat_assist_port: 12_000,
        lease_secs: DEFAULT_LEASE_SECS,
        relay_port_range_start: Some(12_000),
        relay_port_range_end: Some(12_127),
        publish_interval_secs: DEFAULT_PUBLISH_INTERVAL_SECS,
        max_active_sessions: 32,
        max_sessions_per_requester: DEFAULT_MAX_SESSIONS_PER_REQUESTER,
        max_bytes_per_session: DEFAULT_MAX_BYTES_PER_SESSION,
        max_forward_bps: None,
        price_hint_msats: None,
        state_file: None,
    };

    let error = relay_port_range(&args).expect_err("overlapping nat assist should fail");
    assert!(
        error
            .to_string()
            .contains("nat assist port 12000 overlaps relay port range 12000-12127")
    );
}

#[tokio::test]
async fn relay_port_allocator_skips_busy_ports() {
    let (start, mut reservations) = find_reserved_udp_range(4);
    let busy_socket = reservations.remove(0);
    drop(reservations);

    let range = RelayPortRange::new(start, start + 3).expect("range");
    let mut allocator = RelayPortAllocator::new(range);
    let (_, _, requester_endpoint, target_endpoint) = allocator
        .bind_pair(IpAddr::V4(Ipv4Addr::LOCALHOST), "127.0.0.1")
        .expect("bind relay pair");

    let requester_port = requester_endpoint
        .rsplit(':')
        .next()
        .expect("requester port")
        .parse::<u16>()
        .expect("requester port number");
    let target_port = target_endpoint
        .rsplit(':')
        .next()
        .expect("target port")
        .parse::<u16>()
        .expect("target port number");

    assert_ne!(requester_port, start);
    assert_ne!(target_port, start);
    assert_ne!(requester_port, target_port);

    drop(busy_socket);
}

#[test]
fn relay_port_allocator_rejects_exhausted_range() {
    let (start, reservations) = find_reserved_udp_range(2);
    let range = RelayPortRange::new(start, start + 1).expect("range");
    let mut allocator = RelayPortAllocator::new(range);

    let error = allocator
        .bind_pair(IpAddr::V4(Ipv4Addr::LOCALHOST), "127.0.0.1")
        .expect_err("fully reserved range should fail");
    assert!(error.to_string().contains(&format!(
        "no free relay port pair available in configured range {start}-{}",
        start + 1
    )));

    drop(reservations);
}

#[tokio::test]
async fn bind_relay_leg_pair_uses_configured_range() {
    let (start, reservations) = find_reserved_udp_range(4);
    drop(reservations);

    let allocator = Arc::new(std::sync::Mutex::new(RelayPortAllocator::new(
        RelayPortRange::new(start, start + 3).expect("range"),
    )));

    let (_, _, requester_endpoint, target_endpoint) = bind_relay_leg_pair(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "127.0.0.1",
        Some(&allocator),
    )
    .expect("bind relay pair");

    let requester_port = requester_endpoint
        .rsplit(':')
        .next()
        .expect("requester port")
        .parse::<u16>()
        .expect("requester port number");
    let target_port = target_endpoint
        .rsplit(':')
        .next()
        .expect("target port")
        .parse::<u16>()
        .expect("target port number");

    assert!((start..=start + 3).contains(&requester_port));
    assert!((start..=start + 3).contains(&target_port));
    assert_ne!(requester_port, target_port);
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
