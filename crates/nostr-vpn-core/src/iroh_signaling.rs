use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use iroh::{
    Endpoint, EndpointId,
    endpoint::{Connection, RecvStream},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, broadcast};

use crate::signaling::{SignalEnvelope, SignalPayload, SignalingNetwork};

pub const IROH_SIGNALING_ALPN: &[u8] = b"nostr-vpn/iroh-signaling/1";

#[derive(Debug, Clone)]
struct ConfiguredNetwork {
    network_id: String,
    participants: HashSet<String>,
}

pub struct IrohSignalingClient {
    endpoint: Endpoint,
    own_endpoint_id: String,
    networks: Arc<Vec<ConfiguredNetwork>>,
    participant_ids: Arc<HashSet<String>>,
    connections: Arc<Mutex<HashMap<String, Connection>>>,
    recv_rx: Mutex<broadcast::Receiver<SignalEnvelope>>,
    recv_tx: broadcast::Sender<SignalEnvelope>,
}

impl IrohSignalingClient {
    pub fn new_with_endpoint(endpoint: Endpoint, networks: Vec<SignalingNetwork>) -> Result<Self> {
        let own_endpoint_id = endpoint.id().to_string();
        let networks = normalize_networks(networks)?;
        let participant_ids = networks
            .iter()
            .flat_map(|network| network.participants.iter().cloned())
            .collect::<HashSet<_>>();
        let (recv_tx, recv_rx) = broadcast::channel(2048);
        let connections = Arc::new(Mutex::new(HashMap::new()));
        let client = Self {
            endpoint,
            own_endpoint_id,
            networks: Arc::new(networks),
            participant_ids: Arc::new(participant_ids),
            connections,
            recv_rx: Mutex::new(recv_rx),
            recv_tx,
        };
        client.start_accept_loop();
        Ok(client)
    }

    pub fn endpoint_id(&self) -> &str {
        &self.own_endpoint_id
    }

    pub async fn publish(&self, payload: SignalPayload) -> Result<()> {
        let recipients = self
            .participant_ids
            .iter()
            .filter(|participant| participant.as_str() != self.own_endpoint_id)
            .cloned()
            .collect::<Vec<_>>();
        if recipients.is_empty() {
            return Err(anyhow!(
                "no configured participants to send iroh signaling message to"
            ));
        }
        self.publish_to(payload, &recipients).await
    }

    pub async fn publish_to(&self, payload: SignalPayload, recipients: &[String]) -> Result<()> {
        let recipients = recipients
            .iter()
            .filter(|participant| participant.as_str() != self.own_endpoint_id)
            .filter(|participant| self.participant_ids.contains(participant.as_str()))
            .cloned()
            .collect::<HashSet<_>>();
        if recipients.is_empty() {
            return Err(anyhow!(
                "no configured participants to send iroh signaling message to"
            ));
        }

        let mut delivered = HashSet::new();
        let mut first_error = None;
        for network in self.networks.iter() {
            let network_recipients = recipients
                .iter()
                .filter(|participant| network.participants.contains(participant.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if network_recipients.is_empty() {
                continue;
            }

            match self
                .publish_to_network(payload.clone(), &network.network_id, &network_recipients)
                .await
            {
                Ok(sent) => delivered.extend(sent),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        if delivered == recipients {
            return Ok(());
        }

        if let Some(error) = first_error {
            return Err(error);
        }

        Err(anyhow!("failed to deliver iroh signaling message"))
    }

    pub async fn recv(&self) -> Option<SignalEnvelope> {
        loop {
            let result = {
                let mut recv_rx = self.recv_rx.lock().await;
                recv_rx.recv().await
            };
            match result {
                Ok(message) => return Some(message),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    async fn publish_to_network(
        &self,
        payload: SignalPayload,
        network_id: &str,
        recipients: &[String],
    ) -> Result<HashSet<String>> {
        let envelope = SignalEnvelope {
            network_id: network_id.to_string(),
            sender_pubkey: self.own_endpoint_id.clone(),
            payload,
        };
        let encoded = encode_envelope(&envelope)?;
        let mut delivered = HashSet::new();

        for recipient in recipients {
            let conn = self.connection_for(recipient).await?;
            let mut send = conn.open_uni().await.with_context(|| {
                format!("failed to open iroh stream for participant {recipient}")
            })?;
            send.write_u32(encoded.len() as u32)
                .await
                .with_context(|| format!("failed to write frame length to {recipient}"))?;
            send.write_all(&encoded)
                .await
                .with_context(|| format!("failed to write signaling payload to {recipient}"))?;
            send.finish().with_context(|| {
                format!("failed to finish iroh stream for participant {recipient}")
            })?;
            delivered.insert(recipient.clone());
        }

        Ok(delivered)
    }

    fn start_accept_loop(&self) {
        let endpoint = self.endpoint.clone();
        let recv_tx = self.recv_tx.clone();
        let networks = Arc::clone(&self.networks);
        let participants = Arc::clone(&self.participant_ids);
        let connections = Arc::clone(&self.connections);

        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let recv_tx = recv_tx.clone();
                let networks = Arc::clone(&networks);
                let participants = Arc::clone(&participants);
                let connections = Arc::clone(&connections);
                tokio::spawn(async move {
                    let Ok(connection) = incoming.await else {
                        return;
                    };
                    let remote_id = connection.remote_id().to_string();
                    if !participants.contains(remote_id.as_str()) {
                        return;
                    }
                    connections
                        .lock()
                        .await
                        .insert(remote_id.clone(), connection.clone());
                    drain_connection(
                        connection,
                        remote_id.clone(),
                        networks,
                        participants,
                        recv_tx,
                    )
                    .await;
                    connections.lock().await.remove(&remote_id);
                });
            }
        });
    }

    async fn connection_for(&self, participant: &str) -> Result<Connection> {
        if let Some(connection) = self.connections.lock().await.get(participant).cloned() {
            return Ok(connection);
        }

        let endpoint_id = parse_endpoint_id(participant)?;
        let connection = self
            .endpoint
            .connect(endpoint_id, IROH_SIGNALING_ALPN)
            .await
            .with_context(|| format!("failed to connect to iroh participant {participant}"))?;
        self.connections
            .lock()
            .await
            .insert(participant.to_string(), connection.clone());
        Ok(connection)
    }
}

async fn drain_connection(
    connection: Connection,
    remote_id: String,
    networks: Arc<Vec<ConfiguredNetwork>>,
    participants: Arc<HashSet<String>>,
    recv_tx: broadcast::Sender<SignalEnvelope>,
) {
    while let Ok(mut recv) = connection.accept_uni().await {
        let Ok(envelope) = read_envelope(&mut recv).await else {
            continue;
        };
        if !participants.contains(remote_id.as_str()) {
            continue;
        }
        if envelope.sender_pubkey != remote_id {
            continue;
        }
        if !networks.iter().any(|network| {
            network.network_id == envelope.network_id
                && network.participants.contains(remote_id.as_str())
        }) {
            continue;
        }
        let _ = recv_tx.send(envelope);
    }
}

async fn read_envelope(recv: &mut RecvStream) -> Result<SignalEnvelope> {
    let frame_len = recv
        .read_u32()
        .await
        .context("failed to read iroh signaling frame length")? as usize;
    let mut frame = vec![0_u8; frame_len];
    recv.read_exact(&mut frame)
        .await
        .context("failed to read iroh signaling frame")?;
    serde_json::from_slice(&frame).context("failed to decode iroh signaling payload")
}

fn encode_envelope(envelope: &SignalEnvelope) -> Result<Vec<u8>> {
    serde_json::to_vec(envelope).context("failed to encode iroh signaling payload")
}

fn normalize_networks(networks: Vec<SignalingNetwork>) -> Result<Vec<ConfiguredNetwork>> {
    if networks.is_empty() {
        return Err(anyhow!("at least one signaling network is required"));
    }

    let mut normalized = Vec::with_capacity(networks.len());
    let mut seen_ids = HashSet::new();
    for network in networks {
        let network_id = network.network_id.trim().to_string();
        if network_id.is_empty() {
            return Err(anyhow!("signaling network id must not be empty"));
        }
        if !seen_ids.insert(network_id.clone()) {
            return Err(anyhow!("duplicate signaling network id {network_id}"));
        }

        let mut participants = HashSet::new();
        for participant in network.participants {
            participants.insert(normalize_endpoint_id(&participant)?);
        }

        normalized.push(ConfiguredNetwork {
            network_id,
            participants,
        });
    }
    Ok(normalized)
}

fn normalize_endpoint_id(value: &str) -> Result<String> {
    Ok(parse_endpoint_id(value)?.to_string())
}

fn parse_endpoint_id(value: &str) -> Result<EndpointId> {
    EndpointId::from_str(value.trim())
        .with_context(|| format!("invalid iroh endpoint id {}", value.trim()))
}
