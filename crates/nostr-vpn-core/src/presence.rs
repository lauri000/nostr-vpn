use std::collections::{HashMap, HashSet};

use crate::control::PeerAnnouncement;
use crate::signaling::{SignalPayload, signal_payload_kind};
use tracing::{debug, info};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlPlanePeerState {
    pub latest_announcement: Option<PeerAnnouncement>,
    pub last_hello_at: Option<u64>,
    pub last_announce_at: Option<u64>,
    pub last_disconnect_at: Option<u64>,
    pub last_signal_seen_at: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ControlPlanePeerBook {
    fresh_announcements: HashMap<String, PeerAnnouncement>,
    known_announcements: HashMap<String, PeerAnnouncement>,
    last_signal_seen_at: HashMap<String, u64>,
    last_hello_at: HashMap<String, u64>,
    last_announce_at: HashMap<String, u64>,
    last_disconnect_at: HashMap<String, u64>,
}

// Transitional alias while CLI/runtime call sites migrate to explicit control-plane naming.
pub type PeerPresenceBook = ControlPlanePeerBook;

impl ControlPlanePeerBook {
    pub fn apply_signal(
        &mut self,
        sender_pubkey: impl Into<String>,
        payload: SignalPayload,
        seen_at: u64,
    ) -> bool {
        let sender_pubkey = sender_pubkey.into();
        self.last_signal_seen_at
            .insert(sender_pubkey.clone(), seen_at);

        match payload {
            SignalPayload::Hello => {
                self.last_hello_at.insert(sender_pubkey.clone(), seen_at);
                debug!(
                    sender_pubkey = %sender_pubkey,
                    seen_at,
                    payload_kind = %signal_payload_kind(&SignalPayload::Hello),
                    "control-plane: observed hello without fresh announcement state change"
                );
                false
            }
            SignalPayload::Announce(announcement) => {
                self.last_announce_at
                    .insert(sender_pubkey.clone(), seen_at);
                let node_id = announcement.node_id.clone();
                let timestamp = announcement.timestamp;
                let should_update_known = self
                    .known_announcements
                    .get(&sender_pubkey)
                    .is_none_or(|existing| existing.timestamp <= announcement.timestamp);
                if should_update_known {
                    self.known_announcements
                        .insert(sender_pubkey.clone(), announcement.clone());
                }

                let should_update_fresh = self
                    .fresh_announcements
                    .get(&sender_pubkey)
                    .is_none_or(|existing| existing.timestamp <= announcement.timestamp);
                if should_update_fresh {
                    self.fresh_announcements
                        .insert(sender_pubkey.clone(), announcement);
                }

                if should_update_known || should_update_fresh {
                    info!(
                        sender_pubkey = %sender_pubkey,
                        node_id = %node_id,
                        timestamp,
                        known_updated = should_update_known,
                        fresh_updated = should_update_fresh,
                        "control-plane: announce updated peer state"
                    );
                } else {
                    debug!(
                        sender_pubkey = %sender_pubkey,
                        node_id = %node_id,
                        timestamp,
                        reason = "stale_announcement_timestamp",
                        "control-plane: announce did not update peer state"
                    );
                }
                should_update_fresh
            }
            SignalPayload::Disconnect { node_id } => {
                self.last_disconnect_at
                    .insert(sender_pubkey.clone(), seen_at);
                let fresh_removed = self.fresh_announcements.remove(&sender_pubkey).is_some();
                let known_removed = self.known_announcements.remove(&sender_pubkey).is_some();
                if fresh_removed || known_removed {
                    info!(
                        sender_pubkey = %sender_pubkey,
                        node_id = %node_id,
                        fresh_removed,
                        known_removed,
                        "control-plane: removed peer state on disconnect"
                    );
                } else {
                    debug!(
                        sender_pubkey = %sender_pubkey,
                        node_id = %node_id,
                        reason = "peer_not_present",
                        "control-plane: disconnect did not remove peer state"
                    );
                }
                fresh_removed || known_removed
            }
        }
    }

    pub fn fresh_control_plane_announcements(&self) -> &HashMap<String, PeerAnnouncement> {
        &self.fresh_announcements
    }

    pub fn known_control_plane_announcements(&self) -> &HashMap<String, PeerAnnouncement> {
        &self.known_announcements
    }

    pub fn control_plane_announcement_for(&self, sender_pubkey: &str) -> Option<&PeerAnnouncement> {
        self.fresh_announcements
            .get(sender_pubkey)
            .or_else(|| self.known_announcements.get(sender_pubkey))
    }

    pub fn control_plane_state_for(&self, sender_pubkey: &str) -> ControlPlanePeerState {
        ControlPlanePeerState {
            latest_announcement: self.known_announcements.get(sender_pubkey).cloned(),
            last_hello_at: self.last_hello_at.get(sender_pubkey).copied(),
            last_announce_at: self.last_announce_at.get(sender_pubkey).copied(),
            last_disconnect_at: self.last_disconnect_at.get(sender_pubkey).copied(),
            last_signal_seen_at: self.last_signal_seen_at.get(sender_pubkey).copied(),
        }
    }

    pub fn restore_known(
        &mut self,
        sender_pubkey: impl Into<String>,
        announcement: PeerAnnouncement,
        last_signal_seen_at: Option<u64>,
    ) {
        let sender_pubkey = sender_pubkey.into();
        self.known_announcements
            .insert(sender_pubkey.clone(), announcement);
        if let Some(last_signal_seen_at) = last_signal_seen_at {
            self.last_signal_seen_at
                .insert(sender_pubkey, last_signal_seen_at);
        }
    }

    pub fn last_signal_seen(&self) -> &HashMap<String, u64> {
        &self.last_signal_seen_at
    }

    pub fn last_signal_seen_at(&self, sender_pubkey: &str) -> Option<u64> {
        self.last_signal_seen_at.get(sender_pubkey).copied()
    }

    pub fn prune_stale_control_plane_state(
        &mut self,
        now: u64,
        stale_after_secs: u64,
    ) -> Vec<String> {
        if stale_after_secs == 0 {
            return Vec::new();
        }

        let cutoff = now.saturating_sub(stale_after_secs);
        let mut removed = Vec::new();
        self.fresh_announcements.retain(|sender_pubkey, _announcement| {
            let keep = self
                .last_signal_seen_at
                .get(sender_pubkey)
                .copied()
                .is_some_and(|last_seen| last_seen > cutoff);
            if !keep {
                removed.push(sender_pubkey.clone());
            }
            keep
        });
        removed.sort();
        if !removed.is_empty() {
            info!(
                removed_count = removed.len(),
                removed_participants = ?removed,
                cutoff,
                stale_after_secs,
                "control-plane: pruned stale fresh peer state"
            );
        }
        removed
    }

    pub fn retain_participants(&mut self, participants: &HashSet<String>) {
        self.fresh_announcements
            .retain(|participant, _| participants.contains(participant));
        self.known_announcements
            .retain(|participant, _| participants.contains(participant));
        self.last_signal_seen_at
            .retain(|participant, _| participants.contains(participant));
        self.last_hello_at
            .retain(|participant, _| participants.contains(participant));
        self.last_announce_at
            .retain(|participant, _| participants.contains(participant));
        self.last_disconnect_at
            .retain(|participant, _| participants.contains(participant));
    }

    // Transitional wrappers.
    pub fn active(&self) -> &HashMap<String, PeerAnnouncement> {
        self.fresh_control_plane_announcements()
    }

    pub fn known(&self) -> &HashMap<String, PeerAnnouncement> {
        self.known_control_plane_announcements()
    }

    pub fn announcement_for(&self, sender_pubkey: &str) -> Option<&PeerAnnouncement> {
        self.control_plane_announcement_for(sender_pubkey)
    }

    pub fn last_seen(&self) -> &HashMap<String, u64> {
        self.last_signal_seen()
    }

    pub fn last_seen_at(&self, sender_pubkey: &str) -> Option<u64> {
        self.last_signal_seen_at(sender_pubkey)
    }

    pub fn prune_stale(&mut self, now: u64, stale_after_secs: u64) -> Vec<String> {
        self.prune_stale_control_plane_state(now, stale_after_secs)
    }
}
