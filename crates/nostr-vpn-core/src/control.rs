use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAnnouncement {
    pub node_id: String,
    pub public_key: String,
    pub endpoint: String,
    pub tunnel_ip: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PeerDirectory {
    peers: HashMap<String, PeerAnnouncement>,
}

impl PeerDirectory {
    pub fn apply(&mut self, announcement: PeerAnnouncement) {
        match self.peers.get(&announcement.node_id) {
            Some(existing) if existing.timestamp > announcement.timestamp => {}
            _ => {
                self.peers
                    .insert(announcement.node_id.clone(), announcement);
            }
        }
    }

    pub fn get(&self, node_id: &str) -> Option<&PeerAnnouncement> {
        self.peers.get(node_id)
    }

    pub fn remove(&mut self, node_id: &str) -> Option<PeerAnnouncement> {
        self.peers.remove(node_id)
    }

    pub fn all(&self) -> Vec<PeerAnnouncement> {
        let mut peers: Vec<PeerAnnouncement> = self.peers.values().cloned().collect();
        peers.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        peers
    }
}
