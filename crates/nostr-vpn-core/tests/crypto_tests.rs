#[cfg(not(any(target_os = "android", target_os = "ios")))]
use nostr_vpn_core::crypto::simulate_boringtun_handshake;
use nostr_vpn_core::crypto::{
    decode_private_key, decode_public_key, generate_keypair, public_key_from_private_key,
};

#[test]
fn key_generation_round_trips_through_base64() {
    let keypair = generate_keypair();
    let private_key = decode_private_key(&keypair.private_key).expect("private key should decode");
    let expected_public = public_key_from_private_key(&private_key);

    assert_eq!(keypair.public_key, expected_public);
    decode_public_key(&keypair.public_key).expect("public key should decode");
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[test]
fn boringtun_handshake_completes_between_two_generated_nodes() {
    let first = generate_keypair();
    let second = generate_keypair();

    let transcript = simulate_boringtun_handshake(&first.private_key, &second.private_key)
        .expect("handshake simulation should complete");

    assert!(transcript.initiation_len > 0);
    assert!(transcript.response_len > 0);
    assert!(transcript.keepalive_len > 0);
}
