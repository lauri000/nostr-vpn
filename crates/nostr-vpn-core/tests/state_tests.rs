use nostr_vpn_core::control::{PeerAnnouncement, PeerDirectory};

#[test]
fn newest_peer_announcement_wins() {
    let mut peers = PeerDirectory::default();

    peers.apply(PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: "pk1".to_string(),
        endpoint: "1.2.3.4:51820".to_string(),
        tunnel_ip: "10.44.0.2/32".to_string(),
        timestamp: 1,
    });

    peers.apply(PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: "pk1".to_string(),
        endpoint: "9.9.9.9:51820".to_string(),
        tunnel_ip: "10.44.0.2/32".to_string(),
        timestamp: 3,
    });

    peers.apply(PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: "pk1".to_string(),
        endpoint: "4.4.4.4:51820".to_string(),
        tunnel_ip: "10.44.0.2/32".to_string(),
        timestamp: 2,
    });

    let peer = peers.get("peer-a").expect("peer should exist");
    assert_eq!(peer.endpoint, "9.9.9.9:51820");
}

#[test]
fn peer_can_be_removed() {
    let mut peers = PeerDirectory::default();

    peers.apply(PeerAnnouncement {
        node_id: "peer-a".to_string(),
        public_key: "pk1".to_string(),
        endpoint: "1.2.3.4:51820".to_string(),
        tunnel_ip: "10.44.0.2/32".to_string(),
        timestamp: 1,
    });

    let removed = peers.remove("peer-a");
    assert!(removed.is_some());
    assert!(peers.get("peer-a").is_none());
}
