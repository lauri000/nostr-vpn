use std::time::Duration;

use iroh::{Endpoint, SecretKey, address_lookup::memory::MemoryLookup, endpoint::presets};
use nostr_vpn_core::{
    control::PeerAnnouncement,
    iroh_signaling::{IROH_SIGNALING_ALPN, IrohSignalingClient},
    signaling::{SignalPayload, SignalingNetwork},
};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn announces_over_local_iroh_endpoint_pair() {
    let memory_lookup = MemoryLookup::new();

    let sender_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate(&mut rand::rng()))
        .alpns(vec![IROH_SIGNALING_ALPN.to_vec()])
        .address_lookup(memory_lookup.clone())
        .bind()
        .await
        .expect("sender endpoint");
    let receiver_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate(&mut rand::rng()))
        .alpns(vec![IROH_SIGNALING_ALPN.to_vec()])
        .address_lookup(memory_lookup.clone())
        .bind()
        .await
        .expect("receiver endpoint");

    memory_lookup.add_endpoint_info(sender_endpoint.addr());
    memory_lookup.add_endpoint_info(receiver_endpoint.addr());

    let sender_id = sender_endpoint.id().to_string();
    let receiver_id = receiver_endpoint.id().to_string();
    let networks = vec![SignalingNetwork {
        network_id: "iroh-test".to_string(),
        participants: vec![sender_id.clone(), receiver_id.clone()],
    }];

    let sender = IrohSignalingClient::new_with_endpoint(sender_endpoint, networks.clone())
        .expect("sender client");
    let receiver =
        IrohSignalingClient::new_with_endpoint(receiver_endpoint, networks).expect("receiver");

    let announcement = PeerAnnouncement {
        node_id: "sender-node".to_string(),
        public_key: "sender-public".to_string(),
        endpoint: "127.0.0.1:51820".to_string(),
        local_endpoint: None,
        public_endpoint: None,
        tunnel_ip: "10.44.0.5/32".to_string(),
        advertised_routes: Vec::new(),
        timestamp: 42,
    };

    sender
        .publish(SignalPayload::Announce(announcement.clone()))
        .await
        .expect("publish should succeed");

    let received = timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("timed out waiting for message")
        .expect("message expected");

    assert_eq!(received.network_id, "iroh-test");
    assert_eq!(received.sender_pubkey, sender_id);
    assert_eq!(received.payload, SignalPayload::Announce(announcement));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_is_received_over_local_iroh_endpoint_pair() {
    let memory_lookup = MemoryLookup::new();

    let sender_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate(&mut rand::rng()))
        .alpns(vec![IROH_SIGNALING_ALPN.to_vec()])
        .address_lookup(memory_lookup.clone())
        .bind()
        .await
        .expect("sender endpoint");
    let receiver_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate(&mut rand::rng()))
        .alpns(vec![IROH_SIGNALING_ALPN.to_vec()])
        .address_lookup(memory_lookup.clone())
        .bind()
        .await
        .expect("receiver endpoint");

    memory_lookup.add_endpoint_info(sender_endpoint.addr());
    memory_lookup.add_endpoint_info(receiver_endpoint.addr());

    let networks = vec![SignalingNetwork {
        network_id: "iroh-test-hello".to_string(),
        participants: vec![
            sender_endpoint.id().to_string(),
            receiver_endpoint.id().to_string(),
        ],
    }];

    let sender =
        IrohSignalingClient::new_with_endpoint(sender_endpoint, networks.clone()).expect("sender");
    let receiver =
        IrohSignalingClient::new_with_endpoint(receiver_endpoint, networks).expect("receiver");

    sender
        .publish(SignalPayload::Hello)
        .await
        .expect("hello publish should succeed");

    let received = timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("timed out waiting for hello")
        .expect("message expected");

    assert_eq!(received.payload, SignalPayload::Hello);
}
