use std::net::Ipv4Addr;

use crate::*;

#[test]
fn runtime_local_signal_endpoint_prefers_detected_ipv4_for_private_configured_endpoint() {
    assert_eq!(
        runtime_local_signal_endpoint(
            "192.168.178.55:51820",
            52000,
            Some(Ipv4Addr::new(172, 20, 10, 2)),
        ),
        "172.20.10.2:52000"
    );
    assert_eq!(
        runtime_local_signal_endpoint(
            "127.0.0.1:51820",
            52000,
            Some(Ipv4Addr::new(172, 20, 10, 2)),
        ),
        "172.20.10.2:52000"
    );
}

#[test]
fn runtime_local_signal_endpoint_prefers_detected_ipv4_for_cgnat_configured_endpoint() {
    assert_eq!(
        runtime_local_signal_endpoint(
            "100.110.224.101:51820",
            52000,
            Some(Ipv4Addr::new(192, 168, 178, 80)),
        ),
        "192.168.178.80:52000"
    );
}

#[test]
fn runtime_local_signal_endpoint_keeps_public_configured_endpoint() {
    assert_eq!(
        runtime_local_signal_endpoint(
            "93.184.216.34:51820",
            52000,
            Some(Ipv4Addr::new(172, 20, 10, 2)),
        ),
        "93.184.216.34:52000"
    );
}

#[test]
fn runtime_signal_ipv4_ignores_tunnel_address() {
    assert_eq!(
        runtime_signal_ipv4(Some(Ipv4Addr::new(10, 44, 110, 128)), "10.44.110.128/32"),
        None
    );
    assert_eq!(
        runtime_signal_ipv4(Some(Ipv4Addr::new(192, 168, 178, 80)), "10.44.110.128/32"),
        Some(Ipv4Addr::new(192, 168, 178, 80))
    );
}

#[test]
fn public_endpoint_for_listen_port_requires_matching_discovery_port() {
    let endpoint = DiscoveredPublicSignalEndpoint {
        listen_port: 51820,
        endpoint: "198.51.100.20:43127".to_string(),
    };

    assert_eq!(
        public_endpoint_for_listen_port(Some(&endpoint), 51820),
        Some("198.51.100.20:43127".to_string())
    );
    assert_eq!(
        public_endpoint_for_listen_port(Some(&endpoint), 51821),
        None
    );
}

#[test]
fn mapped_public_signal_endpoint_rejects_cgnat_address() {
    assert_eq!(
        public_signal_endpoint_from_mapping(51820, "100.99.218.131:51821".to_string()),
        None
    );
}

#[test]
fn mapped_public_signal_endpoint_accepts_public_address() {
    assert_eq!(
        public_signal_endpoint_from_mapping(51820, "198.51.100.20:51821".to_string()),
        Some(DiscoveredPublicSignalEndpoint {
            listen_port: 51820,
            endpoint: "198.51.100.20:51821".to_string(),
        })
    );
}

#[test]
fn fallback_public_signal_endpoint_normalizes_stale_port_to_listen_port() {
    let previous = DiscoveredPublicSignalEndpoint {
        listen_port: 51820,
        endpoint: "198.51.100.20:40787".to_string(),
    };

    assert_eq!(
        fallback_public_signal_endpoint(Some(&previous), 51820),
        Some(DiscoveredPublicSignalEndpoint {
            listen_port: 51820,
            endpoint: "198.51.100.20:51820".to_string(),
        })
    );
}

#[test]
fn fallback_public_signal_endpoint_rejects_mismatched_listen_port() {
    let previous = DiscoveredPublicSignalEndpoint {
        listen_port: 51820,
        endpoint: "198.51.100.20:40787".to_string(),
    };

    assert_eq!(
        fallback_public_signal_endpoint(Some(&previous), 52000),
        None
    );
}

#[test]
fn restored_public_signal_endpoint_keeps_exact_previous_public_mapping() {
    let state = DaemonRuntimeState {
        advertised_endpoint: "198.51.100.20:40787".to_string(),
        listen_port: 51820,
        ..Default::default()
    };

    assert_eq!(
        restored_public_signal_endpoint_from_state(Some(&state), 51820),
        Some(DiscoveredPublicSignalEndpoint {
            listen_port: 51820,
            endpoint: "198.51.100.20:40787".to_string(),
        })
    );
}

#[test]
fn restored_public_signal_endpoint_normalizes_when_listen_port_changes() {
    let state = DaemonRuntimeState {
        advertised_endpoint: "198.51.100.20:40787".to_string(),
        listen_port: 51820,
        ..Default::default()
    };

    assert_eq!(
        restored_public_signal_endpoint_from_state(Some(&state), 52000),
        None
    );
}

#[test]
fn peer_announcement_includes_effective_advertised_routes() {
    let mut config = AppConfig::generated();
    config.node.advertise_exit_node = true;
    config.node.advertised_routes = vec!["10.0.0.0/24".to_string()];
    config.ensure_defaults();

    let announcement = build_peer_announcement(&config, 51820, None);

    #[cfg(target_os = "macos")]
    assert_eq!(
        announcement.advertised_routes,
        vec!["10.0.0.0/24".to_string(), "0.0.0.0/0".to_string()]
    );

    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        announcement.advertised_routes,
        vec![
            "10.0.0.0/24".to_string(),
            "0.0.0.0/0".to_string(),
            "::/0".to_string(),
        ]
    );
}

#[test]
fn announcement_fingerprint_changes_when_routes_change() {
    let mut config = AppConfig::generated();
    let initial = build_peer_announcement(&config, 51820, None);
    let initial_fingerprint = announcement_fingerprint(&initial);

    config.node.advertise_exit_node = true;
    let updated = build_peer_announcement(&config, 51820, None);

    assert_ne!(initial_fingerprint, announcement_fingerprint(&updated));
}
