mod support;

use std::time::Duration;

use nostr_sdk::prelude::{
    ClientBuilder, EventBuilder, Keys, Kind, PublicKey, Tag, Timestamp, nip44,
};
use nostr_vpn_core::relay::{RelayAllocationGranted, RelayAllocationRequest};
use nostr_vpn_core::service_signaling::{RelayServiceClient, ServiceEnvelope, ServicePayload};
use nostr_vpn_core::signaling::NOSTR_KIND_NOSTR_VPN;
use tokio::time::timeout;

use crate::support::ws_relay::WsRelay;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_service_messages_flow_between_unknown_pubkeys() {
    let mut relay = WsRelay::new();
    relay.start().await.expect("relay should start");
    let relay_url = relay.url().expect("relay url");

    let requester_keys = Keys::generate();
    let operator_keys = Keys::generate();
    let requester =
        RelayServiceClient::from_secret_key(&requester_keys.secret_key().to_secret_hex())
            .expect("requester client");
    let operator = RelayServiceClient::from_secret_key(&operator_keys.secret_key().to_secret_hex())
        .expect("operator client");

    requester
        .connect(std::slice::from_ref(&relay_url))
        .await
        .expect("requester connect");
    operator
        .connect(std::slice::from_ref(&relay_url))
        .await
        .expect("operator connect");

    tokio::time::sleep(Duration::from_millis(200)).await;

    requester
        .publish_to(
            ServicePayload::RelayAllocationRequest(RelayAllocationRequest {
                request_id: "req-1".to_string(),
                network_id: "mesh-1".to_string(),
                target_pubkey: "peer-b".to_string(),
                requested_at: 42,
            }),
            operator.own_pubkey(),
        )
        .await
        .expect("request publish");

    let received = timeout(Duration::from_secs(5), operator.recv())
        .await
        .expect("timed out waiting for request")
        .expect("request expected");
    assert_eq!(
        received.payload,
        ServicePayload::RelayAllocationRequest(RelayAllocationRequest {
            request_id: "req-1".to_string(),
            network_id: "mesh-1".to_string(),
            target_pubkey: "peer-b".to_string(),
            requested_at: 42,
        })
    );

    operator
        .publish_to(
            ServicePayload::RelayAllocationGranted(RelayAllocationGranted {
                request_id: "req-1".to_string(),
                network_id: "mesh-1".to_string(),
                relay_pubkey: operator.own_pubkey().to_string(),
                requester_ingress_endpoint: "198.51.100.10:45001".to_string(),
                target_ingress_endpoint: "198.51.100.10:45002".to_string(),
                expires_at: 500,
            }),
            requester.own_pubkey(),
        )
        .await
        .expect("grant publish");

    let response = timeout(Duration::from_secs(5), requester.recv())
        .await
        .expect("timed out waiting for response")
        .expect("response expected");
    assert_eq!(
        response.payload,
        ServicePayload::RelayAllocationGranted(RelayAllocationGranted {
            request_id: "req-1".to_string(),
            network_id: "mesh-1".to_string(),
            relay_pubkey: operator.own_pubkey().to_string(),
            requester_ingress_endpoint: "198.51.100.10:45001".to_string(),
            target_ingress_endpoint: "198.51.100.10:45002".to_string(),
            expires_at: 500,
        })
    );

    requester.disconnect().await;
    operator.disconnect().await;
    relay.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_service_ignores_event_when_envelope_sender_pubkey_is_forged() {
    let mut relay = WsRelay::new();
    relay.start().await.expect("relay should start");
    let relay_url = relay.url().expect("relay url");

    let operator_keys = Keys::generate();
    let attacker_keys = Keys::generate();
    let forged_sender_keys = Keys::generate();
    let operator = RelayServiceClient::from_secret_key(&operator_keys.secret_key().to_secret_hex())
        .expect("operator client");

    operator
        .connect(std::slice::from_ref(&relay_url))
        .await
        .expect("operator connect");

    let attacker_client = ClientBuilder::new()
        .signer(attacker_keys.clone())
        .database(nostr_sdk::database::MemoryDatabase::new())
        .build();
    attacker_client
        .add_relay(&relay_url)
        .await
        .expect("attacker add relay");
    attacker_client.connect().await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let recipient_pubkey = PublicKey::from_hex(operator.own_pubkey()).expect("operator pubkey");
    let forged_sender_pubkey = forged_sender_keys.public_key().to_hex();
    let envelope = ServiceEnvelope {
        sender_pubkey: forged_sender_pubkey,
        payload: ServicePayload::RelayAllocationRequest(RelayAllocationRequest {
            request_id: "forged-req".to_string(),
            network_id: "mesh-1".to_string(),
            target_pubkey: "peer-b".to_string(),
            requested_at: 77,
        }),
    };
    let plaintext = serde_json::to_string(&envelope).expect("serialize envelope");
    let encrypted = nip44::encrypt(
        attacker_keys.secret_key(),
        &recipient_pubkey,
        &plaintext,
        nip44::Version::V2,
    )
    .expect("encrypt forged payload");
    let expiration = Timestamp::now() + Duration::from_secs(300);
    let event = EventBuilder::new(
        Kind::from(NOSTR_KIND_NOSTR_VPN),
        encrypted,
        vec![
            Tag::public_key(recipient_pubkey),
            Tag::expiration(expiration),
        ],
    )
    .to_event(&attacker_keys)
    .expect("sign forged event");

    let output = attacker_client
        .send_event(event)
        .await
        .expect("publish forged event");
    assert!(
        !output.success.is_empty(),
        "forged event should still publish so the receiver-side check is exercised"
    );

    let missing = timeout(Duration::from_millis(750), operator.recv()).await;
    assert!(
        missing.is_err(),
        "receiver should ignore service envelope when sender_pubkey does not match event pubkey"
    );

    let _ = attacker_client.disconnect().await;
    operator.disconnect().await;
    relay.stop().await;
}
