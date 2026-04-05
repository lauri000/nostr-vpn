use crate::*;

use nostr_sdk::prelude::Keys;

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
