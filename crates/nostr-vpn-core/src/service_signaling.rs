use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};

use crate::relay::{
    RelayAllocationGranted, RelayAllocationRejected, RelayAllocationRequest, RelayProbeGranted,
    RelayProbeRejected, RelayProbeRequest,
};
use crate::signaling::NOSTR_KIND_NOSTR_VPN;

const SIGNAL_EXPIRATION_SECS: u64 = 300;
const SIGNAL_PRIVATE_LOOKBACK_SECS: u64 = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServicePayload {
    RelayAllocationRequest(RelayAllocationRequest),
    RelayAllocationGranted(RelayAllocationGranted),
    RelayAllocationRejected(RelayAllocationRejected),
    RelayProbeRequest(RelayProbeRequest),
    RelayProbeGranted(RelayProbeGranted),
    RelayProbeRejected(RelayProbeRejected),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEnvelope {
    pub sender_pubkey: String,
    pub payload: ServicePayload,
}

pub struct RelayServiceClient {
    own_pubkey: String,
    keys: Keys,
    client: Client,
    connected: AtomicBool,
    recv_rx: Mutex<broadcast::Receiver<ServiceEnvelope>>,
    recv_tx: broadcast::Sender<ServiceEnvelope>,
}

impl RelayServiceClient {
    pub fn from_secret_key(secret_key: &str) -> Result<Self> {
        let keys = Keys::parse(secret_key).context("invalid nostr secret key")?;
        let own_pubkey = keys.public_key().to_hex();
        let client = ClientBuilder::new()
            .signer(keys.clone())
            .database(nostr_sdk::database::MemoryDatabase::new())
            .build();
        let (recv_tx, recv_rx) = broadcast::channel(2048);

        Ok(Self {
            own_pubkey,
            keys,
            client,
            connected: AtomicBool::new(false),
            recv_rx: Mutex::new(recv_rx),
            recv_tx,
        })
    }

    pub fn own_pubkey(&self) -> &str {
        &self.own_pubkey
    }

    pub async fn connect(&self, relays: &[String]) -> Result<()> {
        for relay in relays {
            self.client
                .add_relay(relay)
                .await
                .with_context(|| format!("failed to add relay {relay}"))?;
        }

        self.client.connect().await;
        self.client
            .subscribe(
                vec![
                    Filter::new()
                        .kind(service_event_kind())
                        .custom_tag(
                            SingleLetterTag::lowercase(Alphabet::P),
                            vec![self.own_pubkey.clone()],
                        )
                        .since(
                            Timestamp::now() - Duration::from_secs(SIGNAL_PRIVATE_LOOKBACK_SECS),
                        ),
                ],
                None,
            )
            .await
            .context("failed to subscribe to relay service events")?;

        self.start_event_forwarder();
        self.connected.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub async fn disconnect(&self) {
        self.connected.store(false, Ordering::Relaxed);
        let _ = self.client.disconnect().await;
    }

    pub async fn publish_to(&self, payload: ServicePayload, recipient: &str) -> Result<()> {
        if !self.connected.load(Ordering::Relaxed) {
            return Err(anyhow!("client not connected"));
        }

        let recipient_pubkey = PublicKey::from_hex(recipient)
            .with_context(|| format!("invalid recipient pubkey {recipient}"))?;
        let envelope = ServiceEnvelope {
            sender_pubkey: self.own_pubkey.clone(),
            payload,
        };
        let content =
            serde_json::to_string(&envelope).context("failed to serialize service envelope")?;
        let encrypted_content = nip44::encrypt(
            self.keys.secret_key(),
            &recipient_pubkey,
            &content,
            nip44::Version::V2,
        )
        .context("failed to encrypt service payload")?;
        let expiration = Timestamp::now() + Duration::from_secs(SIGNAL_EXPIRATION_SECS);
        let tags = vec![
            Tag::public_key(recipient_pubkey),
            Tag::expiration(expiration),
        ];
        let event = EventBuilder::new(service_event_kind(), encrypted_content, tags)
            .to_event(&self.keys)
            .context("failed to sign private service event")?;

        match self.client.send_event(event).await {
            Ok(output) if !output.success.is_empty() => Ok(()),
            Ok(_) => Err(anyhow!("private service event rejected by all relays")),
            Err(error) => Err(anyhow!(error).context("failed to publish private service event")),
        }
    }

    pub async fn recv(&self) -> Option<ServiceEnvelope> {
        let mut rx = self.recv_rx.lock().await;
        loop {
            match rx.recv().await {
                Ok(msg) => return Some(msg),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    fn start_event_forwarder(&self) {
        let mut notifications = self.client.notifications();
        let own_pubkey = self.own_pubkey.clone();
        let keys = self.keys.clone();
        let recv_tx = self.recv_tx.clone();

        tokio::spawn(async move {
            loop {
                let notification = match notifications.recv().await {
                    Ok(notification) => notification,
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                };

                let RelayPoolNotification::Event { event, .. } = notification else {
                    continue;
                };
                if event.kind != service_event_kind() || event.pubkey.to_hex() == own_pubkey {
                    continue;
                }

                let Some(recipient_pubkey) = first_tag_value(&event, "p") else {
                    continue;
                };
                if recipient_pubkey != own_pubkey {
                    continue;
                }

                let plaintext =
                    match nip44::decrypt(keys.secret_key(), &event.pubkey, &event.content) {
                        Ok(plaintext) => plaintext,
                        Err(_) => continue,
                    };
                let Ok(envelope) = serde_json::from_str::<ServiceEnvelope>(&plaintext) else {
                    continue;
                };
                if envelope.sender_pubkey != event.pubkey.to_hex() {
                    continue;
                }
                let _ = recv_tx.send(envelope);
            }
        });
    }
}

fn service_event_kind() -> Kind {
    Kind::from(NOSTR_KIND_NOSTR_VPN)
}

fn first_tag_value(event: &Event, tag_name: &str) -> Option<String> {
    event.tags.iter().find_map(|tag| {
        let values = tag.as_slice();
        (values.len() >= 2 && values[0] == tag_name).then(|| values[1].to_string())
    })
}
