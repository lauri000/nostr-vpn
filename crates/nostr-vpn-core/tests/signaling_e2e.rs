mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use nostr_sdk::prelude::{
    ClientBuilder, EventBuilder, Keys, Kind, PublicKey, Tag, Timestamp, nip44,
};
use nostr_vpn_core::control::PeerAnnouncement;
use nostr_vpn_core::join_requests::{MeshJoinRequest, publish_join_request};
use nostr_vpn_core::signaling::{
    NOSTR_KIND_NOSTR_VPN, NostrSignalingClient, SignalEnvelope, SignalPayload, SignalingNetwork,
};
use tokio::time::timeout;

use crate::support::ws_relay::WsRelay;

const LEGACY_SIGNAL_KIND: u16 = 31_990;

#[path = "signaling_e2e/local_relay_signaling.rs"]
mod local_relay_signaling;

#[path = "signaling_e2e/relay_payloads.rs"]
mod relay_payloads;

#[path = "signaling_e2e/multi_network_signaling.rs"]
mod multi_network_signaling;
