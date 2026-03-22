use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use iroh::{Endpoint, EndpointAddr, endpoint::Connection};
use tokio::sync::{Mutex, mpsc};

pub(crate) struct IrohWireGuardTransport {
    endpoint: Endpoint,
    peers: HashMap<String, EndpointAddr>,
    connections: Arc<Mutex<HashMap<String, Connection>>>,
    recv_tx: mpsc::Sender<IrohWireGuardDatagram>,
    recv_rx: Mutex<mpsc::Receiver<IrohWireGuardDatagram>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IrohWireGuardDatagram {
    pub participant_pubkey: String,
    pub payload: Vec<u8>,
}

impl IrohWireGuardTransport {
    pub(crate) const ALPN: &[u8] = b"nostr-vpn/iroh-wireguard/1";

    pub(crate) async fn new(
        endpoint: Endpoint,
        peers: HashMap<String, EndpointAddr>,
    ) -> Result<Self> {
        let participant_by_endpoint_id = peers
            .iter()
            .map(|(participant, addr)| (addr.id.to_string(), participant.clone()))
            .collect::<HashMap<_, _>>();
        let participant_by_endpoint_id = Arc::new(participant_by_endpoint_id);
        let connections = Arc::new(Mutex::new(HashMap::new()));
        let (recv_tx, recv_rx) = mpsc::channel(2048);

        spawn_accept_loop(
            endpoint.clone(),
            Arc::clone(&participant_by_endpoint_id),
            Arc::clone(&connections),
            recv_tx.clone(),
        );

        Ok(Self {
            endpoint,
            peers,
            connections,
            recv_tx,
            recv_rx: Mutex::new(recv_rx),
        })
    }

    pub(crate) async fn send(&self, participant_pubkey: &str, payload: Vec<u8>) -> Result<()> {
        let connection = self.connection_for(participant_pubkey).await?;
        let max_datagram_size = connection
            .max_datagram_size()
            .ok_or_else(|| anyhow!("iroh peer {participant_pubkey} does not support datagrams"))?;
        if payload.len() > max_datagram_size {
            return Err(anyhow!(
                "payload too large for iroh datagram transport: {} > {}",
                payload.len(),
                max_datagram_size
            ));
        }
        connection
            .send_datagram(Bytes::from(payload))
            .with_context(|| format!("failed to send iroh datagram to {participant_pubkey}"))?;
        Ok(())
    }

    pub(crate) async fn recv(&self) -> Option<IrohWireGuardDatagram> {
        let mut recv_rx = self.recv_rx.lock().await;
        recv_rx.recv().await
    }

    async fn connection_for(&self, participant_pubkey: &str) -> Result<Connection> {
        if let Some(connection) = self
            .connections
            .lock()
            .await
            .get(participant_pubkey)
            .cloned()
        {
            return Ok(connection);
        }

        let endpoint_addr = self
            .peers
            .get(participant_pubkey)
            .cloned()
            .ok_or_else(|| anyhow!("unknown iroh peer {participant_pubkey}"))?;
        let connection = self
            .endpoint
            .connect(endpoint_addr, Self::ALPN)
            .await
            .with_context(|| format!("failed to connect to iroh peer {participant_pubkey}"))?;

        self.connections
            .lock()
            .await
            .insert(participant_pubkey.to_string(), connection.clone());

        spawn_connection_reader(
            participant_pubkey.to_string(),
            connection.clone(),
            Arc::clone(&self.connections),
            self.recv_tx.clone(),
        );

        Ok(connection)
    }
}

fn spawn_accept_loop(
    endpoint: Endpoint,
    participant_by_endpoint_id: Arc<HashMap<String, String>>,
    connections: Arc<Mutex<HashMap<String, Connection>>>,
    recv_tx: mpsc::Sender<IrohWireGuardDatagram>,
) {
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let participant_by_endpoint_id = Arc::clone(&participant_by_endpoint_id);
            let connections = Arc::clone(&connections);
            let recv_tx = recv_tx.clone();
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                let remote_id = connection.remote_id().to_string();
                let Some(participant) = participant_by_endpoint_id.get(&remote_id).cloned() else {
                    return;
                };
                connections
                    .lock()
                    .await
                    .insert(participant.clone(), connection.clone());
                spawn_connection_reader(participant, connection, connections, recv_tx);
            });
        }
    });
}

fn spawn_connection_reader(
    participant_pubkey: String,
    connection: Connection,
    connections: Arc<Mutex<HashMap<String, Connection>>>,
    recv_tx: mpsc::Sender<IrohWireGuardDatagram>,
) {
    tokio::spawn(async move {
        loop {
            let Ok(payload) = connection.read_datagram().await else {
                break;
            };
            if recv_tx
                .send(IrohWireGuardDatagram {
                    participant_pubkey: participant_pubkey.clone(),
                    payload: payload.to_vec(),
                })
                .await
                .is_err()
            {
                break;
            }
        }
        connections.lock().await.remove(&participant_pubkey);
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    use iroh::{
        Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets,
        test_utils::run_relay_server, tls::CaRootsConfig,
    };
    use nostr_vpn_core::crypto::generate_keypair;
    use tokio::time::timeout;

    use super::IrohWireGuardTransport;
    use crate::userspace_wg::{UserspaceWireGuardPeerConfig, UserspaceWireGuardRuntime};

    fn ipv4_packet(dst: Ipv4Addr) -> Vec<u8> {
        let payload = [0xde, 0xad, 0xbe, 0xef];
        let total_len = 20 + payload.len();
        let mut packet = vec![0_u8; total_len];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        packet[8] = 64;
        packet[9] = 17;
        packet[12..16].copy_from_slice(&Ipv4Addr::new(10, 44, 10, 1).octets());
        packet[16..20].copy_from_slice(&dst.octets());
        packet[20..].copy_from_slice(&payload);
        packet
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn userspace_wireguard_packets_round_trip_over_relay_only_iroh_datagrams() {
        let (relay_map, relay_url, _server) = run_relay_server().await.expect("relay");

        let alice_endpoint = Endpoint::builder(presets::N0)
            .secret_key(SecretKey::generate(&mut rand::rng()))
            .alpns(vec![IrohWireGuardTransport::ALPN.to_vec()])
            .ca_roots_config(CaRootsConfig::insecure_skip_verify())
            .clear_ip_transports()
            .relay_mode(RelayMode::Custom(relay_map.clone()))
            .bind()
            .await
            .expect("alice endpoint");
        let bob_endpoint = Endpoint::builder(presets::N0)
            .secret_key(SecretKey::generate(&mut rand::rng()))
            .alpns(vec![IrohWireGuardTransport::ALPN.to_vec()])
            .ca_roots_config(CaRootsConfig::insecure_skip_verify())
            .clear_ip_transports()
            .relay_mode(RelayMode::Custom(relay_map))
            .bind()
            .await
            .expect("bob endpoint");

        let alice_addr = EndpointAddr::new(alice_endpoint.id()).with_relay_url(relay_url.clone());
        let bob_addr = EndpointAddr::new(bob_endpoint.id()).with_relay_url(relay_url);

        let alice_transport = timeout(
            Duration::from_secs(5),
            IrohWireGuardTransport::new(
                alice_endpoint,
                HashMap::from([("bob".to_string(), bob_addr)]),
            ),
        )
        .await
        .expect("timed out creating alice transport")
        .expect("alice transport");
        let bob_transport = timeout(
            Duration::from_secs(5),
            IrohWireGuardTransport::new(
                bob_endpoint,
                HashMap::from([("alice".to_string(), alice_addr)]),
            ),
        )
        .await
        .expect("timed out creating bob transport")
        .expect("bob transport");

        let alice = generate_keypair();
        let bob = generate_keypair();
        let alice_runtime_endpoint = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 43101));
        let bob_runtime_endpoint = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 43102));
        let tunneled_packet = ipv4_packet(Ipv4Addr::new(10, 44, 33, 8));

        let mut alice_runtime = UserspaceWireGuardRuntime::new(
            &alice.private_key,
            vec![UserspaceWireGuardPeerConfig {
                participant_pubkey: "bob".to_string(),
                public_key_base64: bob.public_key.clone(),
                endpoint: bob_runtime_endpoint,
                allowed_ips: vec!["10.44.33.8/32".to_string()],
            }],
        )
        .expect("alice runtime");
        let mut bob_runtime = UserspaceWireGuardRuntime::new(
            &bob.private_key,
            vec![UserspaceWireGuardPeerConfig {
                participant_pubkey: "alice".to_string(),
                public_key_base64: alice.public_key.clone(),
                endpoint: alice_runtime_endpoint,
                allowed_ips: vec!["10.44.10.1/32".to_string()],
            }],
        )
        .expect("bob runtime");

        let mut network_queue = alice_runtime
            .queue_tunnel_packet(&tunneled_packet)
            .expect("alice should start a handshake");
        let mut delivered = Vec::new();

        for _ in 0..8 {
            if network_queue.is_empty() {
                break;
            }

            let mut next_round = Vec::new();
            for datagram in network_queue {
                if datagram.participant_pubkey == "bob" {
                    timeout(
                        Duration::from_secs(5),
                        alice_transport.send(&datagram.participant_pubkey, datagram.payload),
                    )
                    .await
                    .expect("timed out sending alice relay datagram")
                    .expect("alice transport send");
                    let inbound = timeout(Duration::from_secs(5), bob_transport.recv())
                        .await
                        .expect("timed out waiting for bob relay datagram")
                        .expect("bob datagram");
                    let result = bob_runtime
                        .receive_datagram(alice_runtime_endpoint, &inbound.payload)
                        .expect("bob receive");
                    delivered.extend(result.tunnel_packets);
                    next_round.extend(result.outgoing);
                } else {
                    timeout(
                        Duration::from_secs(5),
                        bob_transport.send(&datagram.participant_pubkey, datagram.payload),
                    )
                    .await
                    .expect("timed out sending bob relay datagram")
                    .expect("bob transport send");
                    let inbound = timeout(Duration::from_secs(5), alice_transport.recv())
                        .await
                        .expect("timed out waiting for alice relay datagram")
                        .expect("alice datagram");
                    let result = alice_runtime
                        .receive_datagram(bob_runtime_endpoint, &inbound.payload)
                        .expect("alice receive");
                    delivered.extend(result.tunnel_packets);
                    next_round.extend(result.outgoing);
                }
            }
            network_queue = next_round;
        }

        assert!(
            delivered.iter().any(|packet| packet == &tunneled_packet),
            "expected the tunneled IPv4 packet to be delivered after the relay-only iroh WireGuard handshake"
        );
    }
}
