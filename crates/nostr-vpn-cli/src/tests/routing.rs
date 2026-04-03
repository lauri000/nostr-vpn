use std::collections::HashMap;
use std::net::Ipv4Addr;

use super::{local_endpoints, sample_peer_announcement};
use crate::*;

use nostr_sdk::prelude::Keys;
use nostr_vpn_core::crypto::generate_keypair;
use nostr_vpn_core::paths::PeerPathBook;
use nostr_vpn_core::presence::PeerPresenceBook;
use nostr_vpn_core::relay::RelayAllocationRejectReason;
use nostr_vpn_core::signaling::SignalPayload;

#[test]
fn utun_candidates_expand_for_default_style_names() {
    let candidates = utun_interface_candidates("utun100");
    assert_eq!(candidates.len(), 16);
    assert_eq!(candidates[0], "utun100");
    assert_eq!(candidates[1], "utun101");
    assert_eq!(candidates[15], "utun115");
}

#[test]
fn utun_candidates_keep_custom_iface_as_is() {
    let candidates = utun_interface_candidates("wg0");
    assert_eq!(candidates, vec!["wg0".to_string()]);
}

#[test]
fn uapi_addr_in_use_matcher_detects_common_errnos() {
    assert!(is_uapi_addr_in_use_error("uapi set failed: errno=48"));
    assert!(is_uapi_addr_in_use_error("uapi set failed: errno=98"));
    assert!(!is_uapi_addr_in_use_error("uapi set failed: errno=1"));
}

#[test]
fn endpoint_listen_port_rewrite_updates_socket_port() {
    assert_eq!(
        endpoint_with_listen_port("192.168.1.10:51820", 52000),
        "192.168.1.10:52000"
    );
    assert_eq!(
        endpoint_with_listen_port("[2001:db8::1]:51820", 52000),
        "[2001:db8::1]:52000"
    );
    assert_eq!(
        endpoint_with_listen_port("not-a-socket", 52000),
        "not-a-socket"
    );
}

#[test]
fn local_interface_address_for_tunnel_preserves_host_prefix() {
    assert_eq!(
        local_interface_address_for_tunnel("10.44.0.1/32"),
        "10.44.0.1/32"
    );
    assert_eq!(
        local_interface_address_for_tunnel("10.44.0.1"),
        "10.44.0.1/32"
    );
}

#[test]
fn route_targets_for_tunnel_peers_use_peer_allowed_ips() {
    let routes = route_targets_for_tunnel_peers(&[
        TunnelPeer {
            pubkey_hex: "a".repeat(64),
            endpoint: "203.0.113.10:51820".to_string(),
            allowed_ips: vec!["10.44.0.3/32".to_string()],
        },
        TunnelPeer {
            pubkey_hex: "b".repeat(64),
            endpoint: "203.0.113.11:51820".to_string(),
            allowed_ips: vec!["10.44.0.2/32".to_string(), "10.55.0.0/24".to_string()],
        },
        TunnelPeer {
            pubkey_hex: "c".repeat(64),
            endpoint: "203.0.113.12:51820".to_string(),
            allowed_ips: vec!["10.44.0.2/32".to_string()],
        },
    ]);

    assert_eq!(
        routes,
        vec![
            "10.44.0.2/32".to_string(),
            "10.44.0.3/32".to_string(),
            "10.55.0.0/24".to_string(),
        ]
    );
}

#[test]
fn macos_route_targets_add_default_route_for_selected_exit_peer() {
    let mut config = AppConfig::generated();
    let exit_participant = Keys::generate().public_key().to_hex();
    config.networks[0].participants = vec![exit_participant.clone()];
    config.exit_node = exit_participant.clone();
    config.ensure_defaults();

    let announcements = HashMap::from([(
        exit_participant.clone(),
        PeerAnnouncement {
            node_id: "exit-node".to_string(),
            public_key: generate_keypair().public_key,
            endpoint: "203.0.113.20:51820".to_string(),
            local_endpoint: None,
            public_endpoint: Some("203.0.113.20:51820".to_string()),
            relay_endpoint: None,
            relay_pubkey: None,
            relay_expires_at: None,
            tunnel_ip: "10.44.0.2/32".to_string(),
            advertised_routes: vec!["0.0.0.0/0".to_string(), "10.60.0.0/24".to_string()],
            timestamp: 1,
        },
    )]);

    let planned = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some("192.0.2.10:51820"),
        10,
    )
    .expect("planned tunnel peers");

    let routes = route_targets_for_planned_tunnel_peers(&config, None, &announcements, &planned);

    #[cfg(any(target_os = "macos", test))]
    assert_eq!(
        routes,
        vec![
            "0.0.0.0/0".to_string(),
            "10.44.0.2/32".to_string(),
            "10.60.0.0/24".to_string(),
        ]
    );

    #[cfg(not(any(target_os = "macos", test)))]
    assert_eq!(
        routes,
        vec!["10.44.0.2/32".to_string(), "10.60.0.0/24".to_string()]
    );
}

#[test]
fn route_targets_detect_when_endpoint_bypass_is_required() {
    assert!(!route_targets_require_endpoint_bypass(&[
        "10.44.0.2/32".to_string()
    ]));
    assert!(route_targets_require_endpoint_bypass(&[
        "10.55.0.0/24".to_string()
    ]));
    assert!(route_targets_require_endpoint_bypass(&[
        "0.0.0.0/0".to_string()
    ]));
}

#[test]
fn stun_host_port_supports_default_and_explicit_ports() {
    assert_eq!(
        stun_host_port("stun:stun.iris.to"),
        Some(("stun.iris.to".to_string(), 3478))
    );
    assert_eq!(
        stun_host_port("stun://198.51.100.30:5349"),
        Some(("198.51.100.30".to_string(), 5349))
    );
    assert_eq!(stun_host_port(""), None);
}

#[test]
fn control_plane_bypass_hosts_include_nat_helpers_and_management_hosts() {
    use netdev::interface::flags::{IFF_POINTOPOINT, IFF_UP};
    use netdev::net::device::NetworkDevice;
    use std::net::IpAddr;

    let mut config = AppConfig::generated();
    config.nostr.relays = vec![
        "wss://203.0.113.10".to_string(),
        "wss://198.51.100.20:444".to_string(),
    ];
    config.nat.stun_servers = vec![
        "stun:198.51.100.30:3478".to_string(),
        "stun://203.0.113.10".to_string(),
        "not-a-stun-url".to_string(),
    ];
    config.nat.reflectors = vec!["192.0.2.40:5000".to_string(), "invalid".to_string()];

    let mut physical = NetworkInterface::dummy();
    physical.name = "en0".to_string();
    physical.flags = IFF_UP as u32;
    let mut gateway = NetworkDevice::new();
    gateway.ipv4.push(Ipv4Addr::new(192, 168, 64, 1));
    physical.gateway = Some(gateway);
    physical.dns_servers = vec![
        IpAddr::V4(Ipv4Addr::new(192, 168, 64, 1)),
        IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
    ];

    let mut tunnel = NetworkInterface::dummy();
    tunnel.name = "utun100".to_string();
    tunnel.flags = (IFF_UP | IFF_POINTOPOINT) as u32;
    let mut tunnel_gateway = NetworkDevice::new();
    tunnel_gateway.ipv4.push(Ipv4Addr::new(100, 64, 0, 1));
    tunnel.gateway = Some(tunnel_gateway);
    tunnel.dns_servers = vec![IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];

    let hosts = control_plane_bypass_ipv4_hosts_from_interfaces(&config, &[physical, tunnel]);

    assert_eq!(
        hosts,
        vec![
            Ipv4Addr::new(1, 1, 1, 1),
            Ipv4Addr::new(192, 0, 2, 40),
            Ipv4Addr::new(192, 168, 64, 1),
            Ipv4Addr::new(198, 51, 100, 20),
            Ipv4Addr::new(198, 51, 100, 30),
            Ipv4Addr::new(203, 0, 113, 10),
        ]
    );
}

#[test]
fn runtime_effective_advertised_routes_filter_default_exit_routes_by_platform() {
    let mut config = AppConfig::default();
    config.node.advertise_exit_node = true;
    config.node.advertised_routes = vec!["10.55.0.0/24".to_string()];

    let effective = runtime_effective_advertised_routes(&config);

    #[cfg(target_os = "linux")]
    assert_eq!(
        effective,
        vec![
            "10.55.0.0/24".to_string(),
            "0.0.0.0/0".to_string(),
            "::/0".to_string(),
        ]
    );

    #[cfg(target_os = "macos")]
    assert_eq!(
        effective,
        vec!["10.55.0.0/24".to_string(), "0.0.0.0/0".to_string()]
    );

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert_eq!(effective, vec!["10.55.0.0/24".to_string()]);
}

#[test]
fn selected_exit_node_participant_tracks_supported_platforms() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];
    config.exit_node = participant.clone();

    let announcements = HashMap::from([(
        participant.clone(),
        PeerAnnouncement {
            node_id: "peer-a".to_string(),
            public_key: generate_keypair().public_key,
            endpoint: "203.0.113.20:51820".to_string(),
            local_endpoint: None,
            public_endpoint: Some("203.0.113.20:51820".to_string()),
            relay_endpoint: None,
            relay_pubkey: None,
            relay_expires_at: None,
            tunnel_ip: "10.44.0.2/32".to_string(),
            advertised_routes: vec!["0.0.0.0/0".to_string()],
            timestamp: 10,
        },
    )]);

    let selected = selected_exit_node_participant(&config, None, &announcements);

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert_eq!(selected.as_deref(), Some(participant.as_str()));

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert!(selected.is_none());
}

#[test]
fn macos_route_get_spec_parses_gateway_and_interface() {
    let output = "\
   route to: default\n\
destination: default\n\
   mask: default\n\
gateway: 10.10.243.254\n\
  interface: en0\n";
    let spec = macos_route_get_spec_from_output(output).expect("macOS route spec");
    assert_eq!(spec.gateway.as_deref(), Some("10.10.243.254"));
    assert_eq!(spec.interface, "en0");
}

#[test]
fn split_host_port_keeps_literal_host_without_port() {
    assert_eq!(
        split_host_port("relay.example.com", 443),
        Some(("relay.example.com".to_string(), 443))
    );
    assert_eq!(
        split_host_port("203.0.113.10:51820", 443),
        Some(("203.0.113.10".to_string(), 51820))
    );
}

#[test]
fn linux_ipv4_route_source_uses_tunnel_ipv4_address() {
    assert_eq!(
        linux_ipv4_route_source("10.44.93.37/32"),
        Some("10.44.93.37".to_string())
    );
    assert_eq!(linux_ipv4_route_source("fd00::1/128"), None);
}

#[test]
fn linux_route_target_is_ipv4_detects_ipv4_and_ipv6_targets() {
    assert!(linux_route_target_is_ipv4("0.0.0.0/0"));
    assert!(linux_route_target_is_ipv4("10.44.93.37/32"));
    assert!(!linux_route_target_is_ipv4("::/0"));
}

#[test]
fn linux_exit_node_default_route_families_detect_ipv4_and_ipv6_defaults() {
    let ipv6_only = linux_exit_node_default_route_families(&["::/0".to_string()]);
    assert!(!ipv6_only.ipv4);
    assert!(ipv6_only.ipv6);

    let dual_stack = linux_exit_node_default_route_families(&[
        "10.55.0.0/24".to_string(),
        "0.0.0.0/0".to_string(),
        "::/0".to_string(),
    ]);
    assert!(dual_stack.ipv4);
    assert!(dual_stack.ipv6);
}

#[test]
fn linux_exit_node_ipv6_forward_rules_use_ip6tables_shape() {
    assert_eq!(
        linux_exit_node_firewall_binary(LinuxExitNodeIpFamily::V4),
        "iptables"
    );
    assert_eq!(
        linux_exit_node_firewall_binary(LinuxExitNodeIpFamily::V6),
        "ip6tables"
    );
    assert_eq!(
        linux_exit_node_ipv4_masquerade_rule("eth0", "10.44.0.0/24"),
        vec![
            "POSTROUTING".to_string(),
            "-o".to_string(),
            "eth0".to_string(),
            "-s".to_string(),
            "10.44.0.0/24".to_string(),
            "-m".to_string(),
            "comment".to_string(),
            "--comment".to_string(),
            "nvpn-exit-masq".to_string(),
            "-j".to_string(),
            "MASQUERADE".to_string(),
        ]
    );
    assert_eq!(
        linux_exit_node_forward_in_rule("utun100", LinuxExitNodeIpFamily::V6),
        vec![
            "FORWARD".to_string(),
            "-i".to_string(),
            "utun100".to_string(),
            "-m".to_string(),
            "comment".to_string(),
            "--comment".to_string(),
            "nvpn-exit6-forward-in".to_string(),
            "-j".to_string(),
            "ACCEPT".to_string(),
        ]
    );
    assert_eq!(
        linux_exit_node_forward_out_rule("utun100", LinuxExitNodeIpFamily::V6),
        vec![
            "FORWARD".to_string(),
            "-o".to_string(),
            "utun100".to_string(),
            "-m".to_string(),
            "conntrack".to_string(),
            "--ctstate".to_string(),
            "RELATED,ESTABLISHED".to_string(),
            "-m".to_string(),
            "comment".to_string(),
            "--comment".to_string(),
            "nvpn-exit6-forward-out".to_string(),
            "-j".to_string(),
            "ACCEPT".to_string(),
        ]
    );
}

#[test]
fn linux_exit_node_source_cidr_uses_full_auto_mesh_range() {
    assert_eq!(
        linux_exit_node_source_cidr("10.44.183.163/32"),
        Some("10.44.0.0/16".to_string())
    );
}

#[test]
fn linux_exit_node_source_cidr_preserves_custom_non_mesh_prefixes() {
    assert_eq!(
        linux_exit_node_source_cidr("10.55.7.9/32"),
        Some("10.55.7.0/24".to_string())
    );
}

#[test]
fn parse_exit_node_arg_normalizes_and_clears() {
    let peer = Keys::generate();
    let peer_hex = peer.public_key().to_hex();
    let peer_npub = peer.public_key().to_bech32().expect("peer npub");

    assert_eq!(
        parse_exit_node_arg(&peer_npub).expect("parse exit node"),
        Some(peer_hex)
    );
    assert_eq!(parse_exit_node_arg("off").expect("clear"), None);
    assert_eq!(parse_exit_node_arg("none").expect("clear"), None);
    assert_eq!(parse_exit_node_arg("").expect("clear"), None);
}

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

#[test]
fn planned_tunnel_peers_assign_selected_exit_node_default_route() {
    let mut config = AppConfig::generated();
    let exit_participant = Keys::generate().public_key().to_hex();
    let routed_participant = Keys::generate().public_key().to_hex();
    config.networks[0].participants = vec![exit_participant.clone(), routed_participant.clone()];
    config.exit_node = exit_participant.clone();
    config.ensure_defaults();

    let announcements = HashMap::from([
        (
            exit_participant.clone(),
            PeerAnnouncement {
                node_id: "exit-node".to_string(),
                public_key: generate_keypair().public_key,
                endpoint: "203.0.113.20:51820".to_string(),
                local_endpoint: None,
                public_endpoint: Some("203.0.113.20:51820".to_string()),
                relay_endpoint: None,
                relay_pubkey: None,
                relay_expires_at: None,
                tunnel_ip: "10.44.0.2/32".to_string(),
                advertised_routes: vec![
                    "10.60.0.0/24".to_string(),
                    "0.0.0.0/0".to_string(),
                    "::/0".to_string(),
                ],
                timestamp: 1,
            },
        ),
        (
            routed_participant.clone(),
            PeerAnnouncement {
                node_id: "routed-node".to_string(),
                public_key: generate_keypair().public_key,
                endpoint: "203.0.113.21:51820".to_string(),
                local_endpoint: None,
                public_endpoint: Some("203.0.113.21:51820".to_string()),
                relay_endpoint: None,
                relay_pubkey: None,
                relay_expires_at: None,
                tunnel_ip: "10.44.0.3/32".to_string(),
                advertised_routes: vec!["10.70.0.0/24".to_string()],
                timestamp: 1,
            },
        ),
    ]);

    let planned = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some("192.0.2.10:51820"),
        10,
    )
    .expect("planned tunnel peers");

    let exit_peer = planned
        .iter()
        .find(|planned| planned.participant == exit_participant)
        .expect("exit peer");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert_eq!(
        exit_peer.peer.allowed_ips,
        vec![
            "10.44.0.2/32".to_string(),
            "0.0.0.0/0".to_string(),
            "10.60.0.0/24".to_string(),
        ]
    );
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert_eq!(
        exit_peer.peer.allowed_ips,
        vec!["10.44.0.2/32".to_string(), "10.60.0.0/24".to_string()]
    );

    let routed_peer = planned
        .iter()
        .find(|planned| planned.participant == routed_participant)
        .expect("routed peer");
    assert_eq!(
        routed_peer.peer.allowed_ips,
        vec!["10.44.0.3/32".to_string(), "10.70.0.0/24".to_string()]
    );
}

#[test]
fn planned_tunnel_peers_ignore_default_route_without_selected_exit_node() {
    let mut config = AppConfig::generated();
    let exit_participant = Keys::generate().public_key().to_hex();
    config.networks[0].participants = vec![exit_participant.clone()];
    config.ensure_defaults();

    let announcements = HashMap::from([(
        exit_participant.clone(),
        PeerAnnouncement {
            node_id: "exit-node".to_string(),
            public_key: generate_keypair().public_key,
            endpoint: "203.0.113.20:51820".to_string(),
            local_endpoint: None,
            public_endpoint: Some("203.0.113.20:51820".to_string()),
            relay_endpoint: None,
            relay_pubkey: None,
            relay_expires_at: None,
            tunnel_ip: "10.44.0.2/32".to_string(),
            advertised_routes: vec!["0.0.0.0/0".to_string(), "10.60.0.0/24".to_string()],
            timestamp: 1,
        },
    )]);

    let planned = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some("192.0.2.10:51820"),
        10,
    )
    .expect("planned tunnel peers");

    assert_eq!(
        planned[0].peer.allowed_ips,
        vec!["10.44.0.2/32".to_string(), "10.60.0.0/24".to_string()]
    );
}

#[test]
fn linux_default_route_device_parser_extracts_interface() {
    assert_eq!(
        linux_default_route_device_from_output("default via 198.19.242.2 dev eth0 proto static\n"),
        Some("eth0".to_string())
    );
}

#[test]
fn linux_route_get_parser_extracts_gateway_interface_and_source() {
    let spec = linux_route_get_spec_from_output(
        "10.254.241.10 via 198.19.242.2 dev eth0 src 198.19.242.3 uid 0\n    cache\n",
    )
    .expect("linux route get spec");

    assert_eq!(spec.gateway.as_deref(), Some("198.19.242.2"));
    assert_eq!(spec.dev, "eth0");
    assert_eq!(spec.src.as_deref(), Some("198.19.242.3"));
}

#[test]
fn reuses_running_listen_port_without_rebind() {
    assert!(can_reuse_active_listen_port(true, true, Some(51820), 51820));
    assert!(!can_reuse_active_listen_port(
        true,
        true,
        Some(51820),
        51821
    ));
    assert!(!can_reuse_active_listen_port(
        false,
        true,
        Some(51820),
        51820
    ));
    assert!(!can_reuse_active_listen_port(
        true,
        false,
        Some(51820),
        51820
    ));
    assert!(!can_reuse_active_listen_port(true, true, None, 51820));
}

#[test]
fn tunnel_heartbeat_targets_only_include_peers_without_handshake() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: None,
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 1,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);

    let pending = pending_tunnel_heartbeat_ips(&config, None, &announcements, None);
    assert_eq!(pending, vec![Ipv4Addr::new(10, 44, 0, 2)]);

    let runtime_peers = HashMap::from([(
        key_b64_to_hex(&peer_keys.public_key).expect("peer pubkey hex"),
        WireGuardPeerStatus {
            endpoint: Some("203.0.113.20:51820".to_string()),
            last_handshake_sec: Some(1),
            last_handshake_nsec: Some(0),
            ..WireGuardPeerStatus::default()
        },
    )]);
    let pending = pending_tunnel_heartbeat_ips(&config, None, &announcements, Some(&runtime_peers));
    assert!(pending.is_empty(), "handshaken peer should not be probed");
}

#[test]
fn tunnel_heartbeat_targets_include_peers_with_stale_handshakes() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: None,
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 1,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);
    let runtime_peers = HashMap::from([(
        key_b64_to_hex(&peer_keys.public_key).expect("peer pubkey hex"),
        WireGuardPeerStatus {
            endpoint: Some("203.0.113.20:51820".to_string()),
            last_handshake_sec: Some(PEER_ONLINE_GRACE_SECS + 1),
            last_handshake_nsec: Some(0),
            ..WireGuardPeerStatus::default()
        },
    )]);

    let pending = pending_tunnel_heartbeat_ips(&config, None, &announcements, Some(&runtime_peers));
    assert_eq!(
        pending,
        vec![Ipv4Addr::new(10, 44, 0, 2)],
        "stale peers should still get tunnel heartbeats"
    );
}

#[test]
fn relay_connection_action_reconnects_only_when_disconnected() {
    assert_eq!(
        relay_connection_action(true),
        crate::RelayConnectionAction::KeepConnected
    );
    assert_eq!(
        relay_connection_action(false),
        crate::RelayConnectionAction::ReconnectWhenDue
    );
}

#[test]
fn runtime_handshake_updates_path_cache() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: Some("192.168.1.20:51820".to_string()),
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement.clone())]);
    let mut paths = PeerPathBook::default();
    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut paths,
        Some("192.168.1.33:51820"),
        10,
    )
    .expect("initial tunnel peers");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "192.168.1.20:51820");
    paths.note_selected(&participant, &selected[0].endpoint, 10);

    let runtime_peers = HashMap::from([(
        key_b64_to_hex(&peer_keys.public_key).expect("peer pubkey hex"),
        WireGuardPeerStatus {
            endpoint: Some("203.0.113.20:51820".to_string()),
            last_handshake_sec: Some(1),
            last_handshake_nsec: Some(0),
            ..WireGuardPeerStatus::default()
        },
    )]);
    assert!(record_successful_runtime_paths(
        &announcements,
        Some(&runtime_peers),
        &mut paths,
        &["192.168.1.33:51820".to_string()],
        16,
    ));

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut paths,
        Some("192.168.1.33:51820"),
        16,
    )
    .expect("tunnel peers after handshake");
    assert_eq!(selected[0].endpoint, "203.0.113.20:51820");
}

#[test]
fn successful_local_path_rotates_to_public_after_network_change() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: Some("192.168.1.20:51820".to_string()),
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);
    let mut paths = PeerPathBook::default();

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut paths,
        Some("192.168.1.33:51820"),
        10,
    )
    .expect("initial tunnel peers");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "192.168.1.20:51820");
    paths.note_selected(&participant, &selected[0].endpoint, 10);
    assert!(paths.note_success(participant.clone(), &selected[0].endpoint, 11));

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut paths,
        Some("172.20.10.7:51820"),
        12,
    )
    .expect("tunnel peers after network change");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "203.0.113.20:51820");
}

#[test]
fn runtime_endpoint_refresh_requires_cross_subnet_local_drift() {
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "10.254.241.10:51820".to_string(),
        local_endpoint: Some("198.19.241.3:51820".to_string()),
        public_endpoint: Some("10.254.241.10:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: vec!["0.0.0.0/0".to_string()],
        timestamp: 10,
    };

    assert!(runtime_endpoint_requires_refresh(
        "198.19.241.3:51820",
        "10.254.241.10:51820",
        &announcement,
        &["198.19.242.3:51820".to_string()],
    ));
    assert!(!runtime_endpoint_requires_refresh(
        "198.19.241.3:51820",
        "10.254.241.10:51820",
        &announcement,
        &["198.19.241.4:51820".to_string()],
    ));
    assert!(runtime_endpoint_requires_refresh(
        "198.19.242.1:6861",
        "10.254.241.10:51820",
        &announcement,
        &["198.19.242.3:51820".to_string()],
    ));
    assert!(!runtime_endpoint_requires_refresh(
        "203.0.113.20:51820",
        "10.254.241.10:51820",
        &announcement,
        &["198.19.242.3:51820".to_string()],
    ));
}

#[test]
fn runtime_endpoint_refresh_skips_same_subnet_gateway_translation_for_public_peer() {
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "89.27.103.157:51820".to_string(),
        local_endpoint: Some("192.168.178.80:51820".to_string()),
        public_endpoint: Some("89.27.103.157:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: vec!["0.0.0.0/0".to_string()],
        timestamp: 10,
    };

    assert!(!runtime_endpoint_requires_refresh(
        "192.168.64.1:51820",
        "89.27.103.157:51820",
        &announcement,
        &["192.168.64.2:51820".to_string()],
    ));
    assert!(runtime_endpoint_requires_refresh(
        "192.168.64.1:6861",
        "89.27.103.157:51820",
        &announcement,
        &["192.168.64.2:51820".to_string()],
    ));
}

#[test]
fn record_successful_runtime_paths_ignores_cross_subnet_local_runtime_endpoint() {
    let participant = "11".repeat(32);
    let peer_keys = generate_keypair();
    let announcements = HashMap::from([(
        participant,
        PeerAnnouncement {
            node_id: "peer-a".to_string(),
            public_key: peer_keys.public_key.clone(),
            endpoint: "10.254.241.10:51820".to_string(),
            local_endpoint: Some("198.19.241.3:51820".to_string()),
            public_endpoint: Some("10.254.241.10:51820".to_string()),
            relay_endpoint: None,
            relay_pubkey: None,
            relay_expires_at: None,
            tunnel_ip: "10.44.0.2/32".to_string(),
            advertised_routes: vec!["0.0.0.0/0".to_string()],
            timestamp: 10,
        },
    )]);
    let runtime_peers = HashMap::from([(
        key_b64_to_hex(&peer_keys.public_key).expect("peer pubkey hex"),
        WireGuardPeerStatus {
            endpoint: Some("198.19.241.3:51820".to_string()),
            last_handshake_sec: Some(1),
            last_handshake_nsec: Some(0),
            ..WireGuardPeerStatus::default()
        },
    )]);
    let mut paths = PeerPathBook::default();

    assert!(!record_successful_runtime_paths(
        &announcements,
        Some(&runtime_peers),
        &mut paths,
        &["198.19.242.3:51820".to_string()],
        12,
    ));
}

#[test]
fn runtime_peer_endpoint_refresh_waits_for_handshake() {
    let participant = "11".repeat(32);
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "10.254.241.10:51820".to_string(),
        local_endpoint: Some("198.19.241.3:51820".to_string()),
        public_endpoint: Some("10.254.241.10:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: vec!["0.0.0.0/0".to_string()],
        timestamp: 10,
    };
    let planned = vec![PlannedTunnelPeer {
        participant: participant.clone(),
        endpoint: "10.254.241.10:51820".to_string(),
        peer: TunnelPeer {
            pubkey_hex: key_b64_to_hex(&announcement.public_key).expect("peer pubkey hex"),
            endpoint: "10.254.241.10:51820".to_string(),
            allowed_ips: vec!["10.44.0.2/32".to_string()],
        },
    }];
    let announcements = HashMap::from([(participant, announcement)]);
    let runtime_peers = HashMap::from([(
        planned[0].peer.pubkey_hex.clone(),
        WireGuardPeerStatus {
            endpoint: Some("198.19.241.3:51820".to_string()),
            last_handshake_sec: None,
            last_handshake_nsec: None,
            ..WireGuardPeerStatus::default()
        },
    )]);

    assert!(!runtime_peer_endpoints_require_refresh(
        &planned,
        &announcements,
        Some(&runtime_peers),
        &["198.19.242.3:51820".to_string()],
    ));
}

#[test]
fn cached_successful_endpoint_survives_announcement_flap_until_path_cache_expires() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.nat.enabled = false;
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let original = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: Some("192.168.1.20:51820".to_string()),
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let flapped = PeerAnnouncement {
        public_endpoint: None,
        endpoint: "192.168.1.20:51820".to_string(),
        local_endpoint: Some("192.168.1.20:51820".to_string()),
        timestamp: 20,
        ..original.clone()
    };

    let mut paths = PeerPathBook::default();
    let original_announcements = HashMap::from([(participant.clone(), original)]);
    let runtime_peers = HashMap::from([(
        key_b64_to_hex(&peer_keys.public_key).expect("peer pubkey hex"),
        WireGuardPeerStatus {
            endpoint: Some("203.0.113.20:51820".to_string()),
            last_handshake_sec: Some(1),
            last_handshake_nsec: Some(0),
            ..WireGuardPeerStatus::default()
        },
    )]);
    assert!(record_successful_runtime_paths(
        &original_announcements,
        Some(&runtime_peers),
        &mut paths,
        &["10.0.0.33:51820".to_string()],
        12,
    ));

    let flapped_announcements = HashMap::from([(participant.clone(), flapped.clone())]);
    let selected = planned_tunnel_peers(
        &config,
        None,
        &flapped_announcements,
        &mut paths,
        Some("10.0.0.33:51820"),
        21,
    )
    .expect("cached tunnel peers");
    assert_eq!(selected[0].endpoint, "203.0.113.20:51820");

    paths.prune_stale(200, peer_path_cache_timeout_secs(5));

    let selected = planned_tunnel_peers(
        &config,
        None,
        &flapped_announcements,
        &mut paths,
        Some("10.0.0.33:51820"),
        200,
    )
    .expect("fallback tunnel peers");
    assert_eq!(selected[0].endpoint, "192.168.1.20:51820");
}

#[test]
fn nat_remote_peer_waits_for_public_endpoint_before_runtime_apply() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.nat.enabled = true;
    config.node.endpoint = "198.19.241.3:51820".to_string();
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "198.19.242.3:51820".to_string(),
        local_endpoint: Some("198.19.242.3:51820".to_string()),
        public_endpoint: None,
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some("198.19.241.3:51820"),
        10,
    )
    .expect("planned tunnel peers");
    assert!(selected.is_empty());
    assert!(nat_punch_targets(&config, None, &announcements, 51820).is_empty());
}

#[test]
fn nat_same_subnet_peer_can_use_local_endpoint_without_public_signal() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.nat.enabled = true;
    config.node.endpoint = "198.19.241.3:51820".to_string();
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "198.19.241.11:51820".to_string(),
        local_endpoint: Some("198.19.241.11:51820".to_string()),
        public_endpoint: None,
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some("198.19.241.3:51820"),
        10,
    )
    .expect("planned tunnel peers");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "198.19.241.11:51820");
    assert!(
        nat_punch_targets_for_local_endpoint(&config, None, &announcements, "198.19.241.3:51820")
            .is_empty(),
        "same-subnet peer should not trigger nat punch"
    );
}

#[test]
fn nat_punch_targets_keep_stale_exit_peer_even_when_another_peer_is_online() {
    let mut config = AppConfig::generated();
    let online = "11".repeat(32);
    let stale = "22".repeat(32);
    config.nat.enabled = true;
    config.node.endpoint = "198.19.241.3:51820".to_string();
    config.networks[0].participants = vec![online.clone(), stale.clone()];

    let online_keys = generate_keypair();
    let stale_keys = generate_keypair();
    let announcements = HashMap::from([
        (
            online.clone(),
            PeerAnnouncement {
                node_id: "peer-online".to_string(),
                public_key: online_keys.public_key.clone(),
                endpoint: "203.0.113.20:51820".to_string(),
                local_endpoint: None,
                public_endpoint: Some("203.0.113.20:51820".to_string()),
                relay_endpoint: None,
                relay_pubkey: None,
                relay_expires_at: None,
                tunnel_ip: "10.44.0.2/32".to_string(),
                advertised_routes: Vec::new(),
                timestamp: 10,
            },
        ),
        (
            stale.clone(),
            PeerAnnouncement {
                node_id: "peer-stale".to_string(),
                public_key: stale_keys.public_key.clone(),
                endpoint: "203.0.113.21:51820".to_string(),
                local_endpoint: None,
                public_endpoint: Some("203.0.113.21:51820".to_string()),
                relay_endpoint: None,
                relay_pubkey: None,
                relay_expires_at: None,
                tunnel_ip: "10.44.0.3/32".to_string(),
                advertised_routes: vec!["0.0.0.0/0".to_string()],
                timestamp: 10,
            },
        ),
    ]);
    let runtime_peers = HashMap::from([
        (
            key_b64_to_hex(&online_keys.public_key).expect("online peer pubkey hex"),
            WireGuardPeerStatus {
                endpoint: Some("203.0.113.20:51820".to_string()),
                last_handshake_sec: Some(1),
                last_handshake_nsec: Some(0),
                ..WireGuardPeerStatus::default()
            },
        ),
        (
            key_b64_to_hex(&stale_keys.public_key).expect("stale peer pubkey hex"),
            WireGuardPeerStatus {
                endpoint: Some("203.0.113.21:51820".to_string()),
                last_handshake_sec: Some(PEER_ONLINE_GRACE_SECS + 1),
                last_handshake_nsec: Some(0),
                ..WireGuardPeerStatus::default()
            },
        ),
    ]);

    assert_eq!(
        pending_nat_punch_targets_for_local_endpoint(
            &config,
            None,
            &announcements,
            Some(&runtime_peers),
            "198.19.241.3:51820",
        ),
        vec!["203.0.113.21:51820".parse().expect("socket addr")],
        "a reachable peer should not suppress NAT punching for a stale exit peer"
    );
}

#[test]
fn cgnat_configured_host_endpoint_still_plans_same_lan_peer() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.nat.enabled = true;
    config.node.endpoint = "100.110.224.101:51820".to_string();
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "192.168.178.44:51820".to_string(),
        local_endpoint: Some("192.168.178.44:51820".to_string()),
        public_endpoint: None,
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.1.158/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);
    let own_local_endpoint = runtime_local_signal_endpoint(
        &config.node.endpoint,
        51820,
        Some(Ipv4Addr::new(192, 168, 178, 80)),
    );

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some(&own_local_endpoint),
        10,
    )
    .expect("planned tunnel peers");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "192.168.178.44:51820");
    assert!(
        nat_punch_targets_for_local_endpoint(&config, None, &announcements, &own_local_endpoint)
            .is_empty(),
        "same-lan peer should not trigger nat punch when local endpoint is known"
    );
}

#[test]
fn secondary_local_subnet_peer_is_planned_without_public_signal() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.nat.enabled = true;
    config.node.endpoint = "192.168.178.80:51820".to_string();
    config.networks[0].participants = vec![participant.clone()];

    let peer_keys = generate_keypair();
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: peer_keys.public_key.clone(),
        endpoint: "10.211.55.3:51820".to_string(),
        local_endpoint: Some("10.211.55.3:51820".to_string()),
        public_endpoint: None,
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.199.77/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);
    let own_local_endpoints = local_endpoints(&["192.168.178.80:51820", "10.211.55.2:51820"]);

    let selected = planned_tunnel_peers_for_local_endpoints(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        &own_local_endpoints,
        10,
    )
    .expect("planned tunnel peers");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "10.211.55.3:51820");
    assert!(
        nat_punch_targets_for_local_endpoints(&config, None, &announcements, &own_local_endpoints)
            .is_empty(),
        "peer reachable on a secondary local subnet should not require nat punch"
    );
}

#[test]
fn explicit_announcement_keeps_local_endpoint_for_private_override() {
    let announcement = crate::build_explicit_peer_announcement(
        "peer-a".to_string(),
        generate_keypair().public_key,
        "10.211.55.3:51820".to_string(),
        "10.211.55.3:51820".to_string(),
        "10.44.199.77/32".to_string(),
        Vec::new(),
    );

    assert_eq!(announcement.endpoint, "10.211.55.3:51820");
    assert_eq!(
        announcement.local_endpoint.as_deref(),
        Some("10.211.55.3:51820")
    );
    assert!(announcement.public_endpoint.is_none());
}

#[test]
fn explicit_announcement_keeps_public_and_local_endpoints_separate() {
    let announcement = crate::build_explicit_peer_announcement(
        "peer-a".to_string(),
        generate_keypair().public_key,
        "203.0.113.20:51820".to_string(),
        "192.168.178.80:51820".to_string(),
        "10.44.0.239/32".to_string(),
        Vec::new(),
    );

    assert_eq!(announcement.endpoint, "203.0.113.20:51820");
    assert_eq!(
        announcement.local_endpoint.as_deref(),
        Some("192.168.178.80:51820")
    );
    assert_eq!(
        announcement.public_endpoint.as_deref(),
        Some("203.0.113.20:51820")
    );
}

#[test]
fn explicit_announcement_preserves_reflected_private_endpoint_from_distinct_subnet() {
    let announcement = crate::build_explicit_peer_announcement(
        "peer-a".to_string(),
        generate_keypair().public_key,
        "10.254.241.10:51820".to_string(),
        "198.19.241.3:51820".to_string(),
        "10.44.0.239/32".to_string(),
        Vec::new(),
    );

    assert_eq!(announcement.endpoint, "10.254.241.10:51820");
    assert_eq!(
        announcement.local_endpoint.as_deref(),
        Some("198.19.241.3:51820")
    );
    assert_eq!(
        announcement.public_endpoint.as_deref(),
        Some("10.254.241.10:51820")
    );
}

#[test]
fn explicit_announcement_can_attach_active_relay_endpoint() {
    let announcement = crate::build_explicit_peer_announcement_with_relay(
        "peer-a".to_string(),
        generate_keypair().public_key,
        "203.0.113.20:51820".to_string(),
        "192.168.178.80:51820".to_string(),
        "10.44.0.239/32".to_string(),
        Vec::new(),
        crate::RelayAnnouncementDetails {
            relay_endpoint: Some("198.51.100.30:40001".to_string()),
            relay_pubkey: Some("relay-pubkey".to_string()),
            relay_expires_at: Some(500),
        },
    );

    assert_eq!(
        announcement.relay_endpoint.as_deref(),
        Some("198.51.100.30:40001")
    );
    assert_eq!(announcement.relay_pubkey.as_deref(), Some("relay-pubkey"));
    assert_eq!(announcement.relay_expires_at, Some(500));
}

#[test]
fn relay_endpoint_is_preferred_when_active() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: Some("192.168.1.20:51820".to_string()),
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: Some("198.51.100.30:40001".to_string()),
        relay_pubkey: Some("relay-pubkey".to_string()),
        relay_expires_at: Some(500),
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some("10.0.0.33:51820"),
        100,
    )
    .expect("planned tunnel peers");

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "198.51.100.30:40001");
}

#[test]
fn expired_relay_endpoint_is_ignored_for_planning() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: Some("192.168.1.20:51820".to_string()),
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: Some("198.51.100.30:40001".to_string()),
        relay_pubkey: Some("relay-pubkey".to_string()),
        relay_expires_at: Some(50),
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);

    let selected = planned_tunnel_peers(
        &config,
        None,
        &announcements,
        &mut PeerPathBook::default(),
        Some("10.0.0.33:51820"),
        100,
    )
    .expect("planned tunnel peers");

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "203.0.113.20:51820");
}

#[test]
fn local_relay_session_overrides_runtime_endpoint() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "203.0.113.20:51820".to_string(),
        local_endpoint: Some("192.168.1.20:51820".to_string()),
        public_endpoint: Some("203.0.113.20:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let announcements = HashMap::from([(participant.clone(), announcement)]);
    let relay_sessions = HashMap::from([(
        participant.clone(),
        ActiveRelaySession {
            relay_pubkey: "relay-pubkey".to_string(),
            local_ingress_endpoint: "198.51.100.30:40001".to_string(),
            advertised_ingress_endpoint: "198.51.100.30:40002".to_string(),
            granted_at: 100,
            verified_at: Some(101),
            expires_at: 500,
        },
    )]);
    let effective =
        crate::effective_peer_announcements_for_runtime(&announcements, &relay_sessions, 100);

    assert_eq!(
        effective
            .get(&participant)
            .and_then(|announcement| announcement.relay_endpoint.as_deref()),
        Some("198.51.100.30:40001")
    );

    let selected = planned_tunnel_peers(
        &config,
        None,
        &effective,
        &mut PeerPathBook::default(),
        Some("10.0.0.33:51820"),
        100,
    )
    .expect("planned tunnel peers");

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].endpoint, "198.51.100.30:40001");
}

#[test]
fn relay_endpoint_preempts_recent_failed_direct_selection() {
    let mut config = AppConfig::generated();
    let participant = "11".repeat(32);
    config.networks[0].participants = vec![participant.clone()];

    let direct_only = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key.clone(),
        endpoint: "10.203.1.11:51820".to_string(),
        local_endpoint: Some("10.203.1.11:51820".to_string()),
        public_endpoint: Some("10.203.1.11:51820".to_string()),
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.0.2/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 100,
    };

    let direct_and_relay = PeerAnnouncement {
        relay_endpoint: Some("10.203.1.2:40001".to_string()),
        relay_pubkey: Some("relay-pubkey".to_string()),
        relay_expires_at: Some(500),
        ..direct_only.clone()
    };

    let mut path_book = PeerPathBook::default();
    let own_local_endpoints = vec!["10.203.1.10:51820".to_string()];
    path_book.refresh_from_announcement(participant.clone(), &direct_only, 100);
    let initially_selected = path_book
        .select_endpoint_for_local_endpoints(
            &participant,
            &direct_only,
            &own_local_endpoints,
            100,
            crate::PEER_PATH_RETRY_AFTER_SECS,
        )
        .expect("initial endpoint");
    assert_eq!(initially_selected, "10.203.1.11:51820");
    path_book.note_selected(participant.clone(), &initially_selected, 100);

    path_book.refresh_from_announcement(participant.clone(), &direct_and_relay, 101);
    let selected_before_retry = path_book
        .select_endpoint_for_local_endpoints(
            &participant,
            &direct_and_relay,
            &own_local_endpoints,
            101,
            crate::PEER_PATH_RETRY_AFTER_SECS,
        )
        .expect("relay endpoint");

    assert_eq!(selected_before_retry, "10.203.1.11:51820");

    let selected_with_relay = path_book
        .select_endpoint_for_local_endpoints(
            &participant,
            &direct_and_relay,
            &own_local_endpoints,
            106,
            crate::PEER_PATH_RETRY_AFTER_SECS,
        )
        .expect("relay endpoint after retry window");

    assert_eq!(selected_with_relay, "10.203.1.2:40001");
}

#[test]
fn relay_candidates_for_participant_skip_self_target_and_limit_count() {
    let participant = "11".repeat(32);
    let own_pubkey = "22".repeat(32);
    let relay_pubkeys = vec![
        participant.clone(),
        own_pubkey.clone(),
        "33".repeat(32),
        "44".repeat(32),
        "55".repeat(32),
        "66".repeat(32),
    ];

    let selected = crate::relay_candidates_for_participant(
        &relay_pubkeys,
        &participant,
        Some(&own_pubkey),
        &HashMap::new(),
        &HashMap::new(),
        100,
    );

    assert_eq!(
        selected,
        vec![
            "3333333333333333333333333333333333333333333333333333333333333333",
            "4444444444444444444444444444444444444444444444444444444444444444",
            "5555555555555555555555555555555555555555555555555555555555555555",
        ]
    );
}

#[test]
fn accept_relay_allocation_grant_queues_standby_after_first_activation() {
    let participant = "11".repeat(32);
    let mut pending_requests = HashMap::from([
        (
            "req-a".to_string(),
            PendingRelayRequest {
                participant: participant.clone(),
                relay_pubkey: "relay-a".to_string(),
                requested_at: 100,
            },
        ),
        (
            "req-b".to_string(),
            PendingRelayRequest {
                participant: participant.clone(),
                relay_pubkey: "relay-b".to_string(),
                requested_at: 100,
            },
        ),
    ]);
    let mut relay_sessions = HashMap::new();
    let mut standby_relay_sessions = HashMap::new();
    let relay_failures = HashMap::new();

    let accepted = crate::accept_relay_allocation_grant(
        RelayAllocationGranted {
            request_id: "req-a".to_string(),
            network_id: "mesh-1".to_string(),
            relay_pubkey: "relay-a".to_string(),
            requester_ingress_endpoint: "198.51.100.10:41001".to_string(),
            target_ingress_endpoint: "198.51.100.10:41002".to_string(),
            expires_at: 500,
        },
        &mut pending_requests,
        &mut relay_sessions,
        &mut standby_relay_sessions,
        &relay_failures,
        200,
    );

    assert_eq!(accepted, RelayGrantAction::Activated(participant.clone()));
    assert_eq!(
        relay_sessions
            .get(&participant)
            .map(|session| session.relay_pubkey.as_str()),
        Some("relay-a")
    );
    assert_eq!(pending_requests.len(), 1);

    pending_requests.insert(
        "req-c".to_string(),
        PendingRelayRequest {
            participant: participant.clone(),
            relay_pubkey: "relay-c".to_string(),
            requested_at: 200,
        },
    );
    let queued = crate::accept_relay_allocation_grant(
        RelayAllocationGranted {
            request_id: "req-b".to_string(),
            network_id: "mesh-1".to_string(),
            relay_pubkey: "relay-b".to_string(),
            requester_ingress_endpoint: "198.51.100.11:42001".to_string(),
            target_ingress_endpoint: "198.51.100.11:42002".to_string(),
            expires_at: 500,
        },
        &mut pending_requests,
        &mut relay_sessions,
        &mut standby_relay_sessions,
        &relay_failures,
        201,
    );

    assert_eq!(queued, RelayGrantAction::QueuedStandby(participant.clone()));
    assert_eq!(
        relay_sessions
            .get(&participant)
            .map(|session| session.relay_pubkey.as_str()),
        Some("relay-a")
    );
    assert_eq!(
        standby_relay_sessions
            .get(&participant)
            .expect("standby relay")
            .iter()
            .map(|session| session.relay_pubkey.as_str())
            .collect::<Vec<_>>(),
        vec!["relay-b"]
    );
}

#[test]
fn reconcile_active_relay_sessions_promotes_verified_standby_after_timeout() {
    let participant = "11".repeat(32);
    let peer_keys = generate_keypair();
    let announcement = sample_peer_announcement(peer_keys.public_key.clone());
    let mut presence = PeerPresenceBook::default();
    assert!(presence.apply_signal(
        participant.clone(),
        SignalPayload::Announce(announcement.clone()),
        100,
    ));

    let mut relay_sessions = HashMap::from([(
        participant.clone(),
        ActiveRelaySession {
            relay_pubkey: "relay-a".to_string(),
            local_ingress_endpoint: "198.51.100.10:41001".to_string(),
            advertised_ingress_endpoint: "198.51.100.10:41002".to_string(),
            granted_at: 200,
            verified_at: None,
            expires_at: 500,
        },
    )]);
    let mut standby_relay_sessions = HashMap::from([(
        participant.clone(),
        vec![ActiveRelaySession {
            relay_pubkey: "relay-b".to_string(),
            local_ingress_endpoint: "198.51.100.20:42001".to_string(),
            advertised_ingress_endpoint: "198.51.100.20:42002".to_string(),
            granted_at: 201,
            verified_at: None,
            expires_at: 500,
        }],
    )]);
    let mut relay_failures = HashMap::new();
    let mut relay_provider_verifications = HashMap::new();
    let mut pending_requests = HashMap::from([(
        "req-z".to_string(),
        PendingRelayRequest {
            participant: participant.clone(),
            relay_pubkey: "relay-z".to_string(),
            requested_at: 205,
        },
    )]);

    let changed = crate::reconcile_active_relay_sessions(
        &presence,
        None,
        &mut relay_sessions,
        &mut standby_relay_sessions,
        &mut relay_failures,
        &mut relay_provider_verifications,
        &mut pending_requests,
        200 + crate::RELAY_SESSION_VERIFY_TIMEOUT_SECS,
    );

    assert_eq!(changed, vec![participant.clone()]);
    assert_eq!(
        relay_sessions
            .get(&participant)
            .map(|session| session.relay_pubkey.as_str()),
        Some("relay-b")
    );
    assert_eq!(
        relay_sessions
            .get(&participant)
            .map(|session| session.granted_at),
        Some(200 + crate::RELAY_SESSION_VERIFY_TIMEOUT_SECS)
    );
    assert!(pending_requests.is_empty());
    assert!(crate::relay_is_in_failure_cooldown(
        &relay_failures,
        &participant,
        "relay-a",
        200 + crate::RELAY_SESSION_VERIFY_TIMEOUT_SECS,
    ));
}

#[test]
fn relay_candidates_for_participant_skip_cooled_down_relays() {
    let participant = "11".repeat(32);
    let relay_pubkeys = vec!["33".repeat(32), "44".repeat(32), "55".repeat(32)];
    let mut relay_failures = HashMap::new();
    relay_failures.insert(
        format!(
            "{}:{}",
            participant, "3333333333333333333333333333333333333333333333333333333333333333"
        ),
        500,
    );

    let selected = crate::relay_candidates_for_participant(
        &relay_pubkeys,
        &participant,
        None,
        &relay_failures,
        &HashMap::new(),
        400,
    );

    assert_eq!(
        selected,
        vec![
            "4444444444444444444444444444444444444444444444444444444444444444",
            "5555555555555555555555555555555555555555555555555555555555555555",
        ]
    );
}

#[test]
fn relay_candidates_prefer_recently_verified_providers() {
    let participant = "11".repeat(32);
    let relay_pubkeys = vec!["33".repeat(32), "44".repeat(32), "55".repeat(32)];
    let relay_provider_verifications = HashMap::from([
        (
            "4444444444444444444444444444444444444444444444444444444444444444".to_string(),
            RelayProviderVerification {
                verified_at: Some(250),
                failure_cooldown_until: None,
                last_failure_at: None,
                last_probe_attempt_at: Some(250),
                consecutive_failures: 0,
            },
        ),
        (
            "3333333333333333333333333333333333333333333333333333333333333333".to_string(),
            RelayProviderVerification {
                verified_at: Some(200),
                failure_cooldown_until: None,
                last_failure_at: None,
                last_probe_attempt_at: Some(200),
                consecutive_failures: 0,
            },
        ),
    ]);

    let selected = crate::relay_candidates_for_participant(
        &relay_pubkeys,
        &participant,
        None,
        &HashMap::new(),
        &relay_provider_verifications,
        300,
    );

    assert_eq!(
        selected,
        vec![
            "4444444444444444444444444444444444444444444444444444444444444444",
            "3333333333333333333333333333333333333333333333333333333333333333",
            "5555555555555555555555555555555555555555555555555555555555555555",
        ]
    );
}

#[test]
fn relay_rejection_marks_provider_and_participant_failed() {
    let participant = "11".repeat(32);
    let relay_pubkey = "33".repeat(32);
    let now = 200;
    let mut pending_requests = HashMap::from([(
        "req-a".to_string(),
        PendingRelayRequest {
            participant: participant.clone(),
            relay_pubkey: relay_pubkey.clone(),
            requested_at: 100,
        },
    )]);
    let mut relay_failures = HashMap::new();
    let mut relay_provider_verifications = HashMap::new();

    let changed = crate::accept_relay_allocation_rejection(
        RelayAllocationRejected {
            request_id: "req-a".to_string(),
            network_id: "mesh-1".to_string(),
            relay_pubkey: relay_pubkey.clone(),
            reason: RelayAllocationRejectReason::OverCapacity,
            retry_after_secs: Some(90),
        },
        &mut pending_requests,
        &mut relay_failures,
        &mut relay_provider_verifications,
        now,
    );

    assert_eq!(changed.as_deref(), Some(participant.as_str()));
    assert!(pending_requests.is_empty());
    assert!(crate::relay_is_in_failure_cooldown(
        &relay_failures,
        &participant,
        &relay_pubkey,
        now
    ));
    assert!(crate::relay_provider_in_failure_cooldown(
        &relay_provider_verifications,
        &relay_pubkey,
        now
    ));
    assert_eq!(
        relay_provider_verifications
            .get(&relay_pubkey)
            .and_then(|verification| verification.failure_cooldown_until),
        Some(now + 90)
    );
}

#[test]
fn matching_peer_subnet_selects_secondary_local_signal_endpoint() {
    let announcement = PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: generate_keypair().public_key,
        endpoint: "10.211.55.3:51820".to_string(),
        local_endpoint: Some("10.211.55.3:51820".to_string()),
        public_endpoint: None,
        relay_endpoint: None,
        relay_pubkey: None,
        relay_expires_at: None,
        tunnel_ip: "10.44.199.77/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 10,
    };
    let own_local_endpoints = local_endpoints(&[
        "192.168.178.80:51820",
        "10.211.55.2:51820",
        "10.37.129.2:51820",
    ]);

    assert_eq!(
        crate::select_local_signal_endpoint_for_peer(&announcement, &own_local_endpoints)
            .as_deref(),
        Some("10.211.55.2:51820")
    );
}

#[test]
fn runtime_magic_dns_records_prefer_live_announcement_tunnel_ip() {
    let mut config = AppConfig::generated();
    config.magic_dns_suffix = "nvpn".to_string();
    config.networks[0].participants =
        vec!["3d332ed94c79863e73ff8af62882de2853c77d6a5c1fe61d7598a90db1fab645".to_string()];
    config.ensure_defaults();
    config
        .set_peer_alias(
            "3d332ed94c79863e73ff8af62882de2853c77d6a5c1fe61d7598a90db1fab645",
            "pig",
        )
        .expect("set alias");

    let mut announcements = HashMap::new();
    announcements.insert(
        "3d332ed94c79863e73ff8af62882de2853c77d6a5c1fe61d7598a90db1fab645".to_string(),
        PeerAnnouncement {
            node_id: "peer-node".to_string(),
            public_key: "pubkey".to_string(),
            endpoint: "192.168.1.55:51820".to_string(),
            local_endpoint: None,
            public_endpoint: None,
            relay_endpoint: None,
            relay_pubkey: None,
            relay_expires_at: None,
            tunnel_ip: "10.44.0.113/32".to_string(),
            advertised_routes: Vec::new(),
            timestamp: 1,
        },
    );

    let records = build_runtime_magic_dns_records(&config, &announcements);
    assert_eq!(
        records.get("pig.nvpn").map(|ip| ip.to_string()),
        Some("10.44.0.113".to_string())
    );
    assert_eq!(
        records.get("pig").map(|ip| ip.to_string()),
        Some("10.44.0.113".to_string())
    );
}

#[test]
fn runtime_magic_dns_records_follow_latest_announcement_ip() {
    let mut config = AppConfig::generated();
    config.magic_dns_suffix = "nvpn".to_string();
    config.networks[0].participants =
        vec!["3d332ed94c79863e73ff8af62882de2853c77d6a5c1fe61d7598a90db1fab645".to_string()];
    config.ensure_defaults();
    config
        .set_peer_alias(
            "3d332ed94c79863e73ff8af62882de2853c77d6a5c1fe61d7598a90db1fab645",
            "pig",
        )
        .expect("set alias");

    let mut announcements = HashMap::new();
    announcements.insert(
        "3d332ed94c79863e73ff8af62882de2853c77d6a5c1fe61d7598a90db1fab645".to_string(),
        PeerAnnouncement {
            node_id: "peer-node".to_string(),
            public_key: "pubkey".to_string(),
            endpoint: "192.168.1.55:51820".to_string(),
            local_endpoint: None,
            public_endpoint: None,
            relay_endpoint: None,
            relay_pubkey: None,
            relay_expires_at: None,
            tunnel_ip: "10.44.0.113/32".to_string(),
            advertised_routes: Vec::new(),
            timestamp: 1,
        },
    );
    let first = build_runtime_magic_dns_records(&config, &announcements);
    assert_eq!(
        first.get("pig.nvpn").map(|ip| ip.to_string()),
        Some("10.44.0.113".to_string())
    );

    announcements.insert(
        "3d332ed94c79863e73ff8af62882de2853c77d6a5c1fe61d7598a90db1fab645".to_string(),
        PeerAnnouncement {
            node_id: "peer-node".to_string(),
            public_key: "pubkey".to_string(),
            endpoint: "192.168.1.55:51820".to_string(),
            local_endpoint: None,
            public_endpoint: None,
            relay_endpoint: None,
            relay_pubkey: None,
            relay_expires_at: None,
            tunnel_ip: "10.44.0.114/32".to_string(),
            advertised_routes: Vec::new(),
            timestamp: 2,
        },
    );
    let second = build_runtime_magic_dns_records(&config, &announcements);
    assert_eq!(
        second.get("pig.nvpn").map(|ip| ip.to_string()),
        Some("10.44.0.114".to_string())
    );
}
