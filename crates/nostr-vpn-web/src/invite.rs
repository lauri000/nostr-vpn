use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use nostr_vpn_core::config::{
    AppConfig, NetworkConfig, normalize_nostr_pubkey, normalize_runtime_network_id,
};
use serde::{Deserialize, Serialize};

use crate::{NETWORK_INVITE_PREFIX, NETWORK_INVITE_VERSION, to_npub};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkInvite {
    v: u8,
    network_name: String,
    network_id: String,
    inviter_npub: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    inviter_node_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    admins: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    participants: Vec<String>,
    relays: Vec<String>,
}

impl NetworkInvite {
    pub(crate) fn network_name(&self) -> &str {
        &self.network_name
    }
}

pub(crate) fn active_network_invite_code(config: &AppConfig) -> Result<String> {
    let active_network = config.active_network();
    let roster = config.shared_network_roster(&active_network.id)?;
    let own_pubkey = config.own_nostr_pubkey_hex().ok();
    let inviter_pubkey = own_pubkey
        .as_deref()
        .filter(|pubkey| config.is_network_admin(&active_network.id, pubkey))
        .map(str::to_string)
        .or_else(|| preferred_join_request_recipient(active_network))
        .or_else(|| active_network.admins.first().cloned())
        .ok_or_else(|| anyhow!("active network has no admin configured"))?;
    let invite = NetworkInvite {
        v: NETWORK_INVITE_VERSION,
        network_name: active_network.name.trim().to_string(),
        network_id: roster.network_id,
        inviter_npub: to_npub(&inviter_pubkey),
        inviter_node_name: if own_pubkey.as_deref() == Some(inviter_pubkey.as_str()) {
            config.node_name.trim().to_string()
        } else {
            config.peer_alias(&inviter_pubkey).unwrap_or_default()
        },
        admins: roster.admins.iter().map(|admin| to_npub(admin)).collect(),
        participants: roster
            .participants
            .iter()
            .map(|participant| to_npub(participant))
            .collect(),
        relays: normalized_invite_relays(&config.nostr.relays)?,
    };
    let encoded = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&invite).context("failed to encode invite JSON")?);
    Ok(format!("{NETWORK_INVITE_PREFIX}{encoded}"))
}

pub(crate) fn parse_network_invite(value: &str) -> Result<NetworkInvite> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("invite code is empty"));
    }

    let mut invite = if trimmed.starts_with('{') {
        serde_json::from_str::<NetworkInvite>(trimmed)
            .context("failed to parse network invite JSON")?
    } else {
        let payload = trimmed
            .strip_prefix(NETWORK_INVITE_PREFIX)
            .unwrap_or(trimmed);
        let decoded = URL_SAFE_NO_PAD
            .decode(payload)
            .context("failed to decode network invite payload")?;
        serde_json::from_slice::<NetworkInvite>(&decoded)
            .context("failed to parse network invite payload")?
    };

    if invite.v != 1 && invite.v != NETWORK_INVITE_VERSION {
        return Err(anyhow!(
            "unsupported invite version {}; expected 1 or {}",
            invite.v,
            NETWORK_INVITE_VERSION
        ));
    }

    invite.network_name = invite.network_name.trim().to_string();
    if invite.network_name.is_empty() {
        return Err(anyhow!("invite network name is empty"));
    }

    invite.network_id = invite.network_id.trim().to_string();
    if invite.network_id.is_empty() {
        return Err(anyhow!("invite network id is empty"));
    }

    invite.inviter_npub = to_npub(&normalize_nostr_pubkey(&invite.inviter_npub)?);
    invite.inviter_node_name = invite.inviter_node_name.trim().to_string();
    invite.admins = normalized_invite_pubkeys(&invite.admins)?;
    if !invite
        .admins
        .iter()
        .any(|admin| admin == &invite.inviter_npub)
    {
        invite.admins.push(invite.inviter_npub.clone());
        invite.admins.sort();
        invite.admins.dedup();
    }
    invite.participants = normalized_invite_pubkeys(&invite.participants)?;
    if invite.participants.is_empty() {
        invite.participants.push(invite.inviter_npub.clone());
    }
    invite.relays = normalized_invite_relays(&invite.relays)?;
    Ok(invite)
}

pub(crate) fn apply_network_invite_to_active_network(
    config: &mut AppConfig,
    invite: &NetworkInvite,
) -> Result<()> {
    let normalized_invite_network_id = normalize_runtime_network_id(&invite.network_id);
    let normalized_inviter_pubkey = normalize_nostr_pubkey(&invite.inviter_npub)?;
    let own_pubkey = config.own_nostr_pubkey_hex().ok();
    let invite_admins = invite
        .admins
        .iter()
        .map(|admin| normalize_nostr_pubkey(admin))
        .collect::<Result<Vec<_>>>()?;
    let invite_participants = invite
        .participants
        .iter()
        .map(|participant| normalize_nostr_pubkey(participant))
        .collect::<Result<Vec<_>>>()?;

    let (target_network_id, reset_membership) = if let Some(existing) =
        config.networks.iter().find(|network| {
            normalize_runtime_network_id(&network.network_id) == normalized_invite_network_id
        }) {
        (existing.id.clone(), false)
    } else if network_should_adopt_invite(config.active_network()) {
        (config.active_network().id.clone(), true)
    } else {
        let network_id = config.add_network(&invite.network_name);
        config.set_network_enabled(&network_id, true)?;
        (network_id, true)
    };

    let should_adopt_name = config
        .network_by_id(&target_network_id)
        .map(network_should_adopt_invite)
        .unwrap_or(false);
    let inviter_already_configured = config
        .network_by_id(&target_network_id)
        .map(|network| {
            network
                .participants
                .iter()
                .any(|participant| participant == &normalized_inviter_pubkey)
                || network
                    .admins
                    .iter()
                    .any(|admin| admin == &normalized_inviter_pubkey)
        })
        .unwrap_or(false);

    config.set_network_enabled(&target_network_id, true)?;
    config.set_network_mesh_id(&target_network_id, &invite.network_id)?;
    if let Some(network) = config.network_by_id_mut(&target_network_id) {
        if reset_membership {
            network.participants.clear();
            network.admins.clear();
            network.shared_roster_updated_at = 0;
            network.shared_roster_signed_by.clear();
        }

        for participant in &invite_participants {
            if own_pubkey.as_deref() == Some(participant.as_str()) {
                continue;
            }
            network.participants.push(participant.clone());
        }
        network.participants.sort();
        network.participants.dedup();

        for admin in &invite_admins {
            network.admins.push(admin.clone());
        }
        if !network
            .admins
            .iter()
            .any(|admin| admin == &normalized_inviter_pubkey)
        {
            network.admins.push(normalized_inviter_pubkey.clone());
        }
        network.admins.sort();
        network.admins.dedup();

        network.invite_inviter = if network
            .admins
            .iter()
            .any(|admin| admin == &normalized_inviter_pubkey)
        {
            normalized_inviter_pubkey.clone()
        } else {
            network.admins.first().cloned().unwrap_or_default()
        };
        if network
            .outbound_join_request
            .as_ref()
            .is_some_and(|request| {
                !network
                    .admins
                    .iter()
                    .any(|admin| admin == &request.recipient)
            })
        {
            network.outbound_join_request = None;
        }
    }

    if !inviter_already_configured && !invite.inviter_node_name.trim().is_empty() {
        let _ = config.set_peer_alias(&normalized_inviter_pubkey, &invite.inviter_node_name);
    }

    if should_adopt_name && let Some(network) = config.network_by_id_mut(&target_network_id) {
        network.name = invite.network_name.trim().to_string();
    }

    for relay in &invite.relays {
        if !config.nostr.relays.iter().any(|existing| existing == relay) {
            config.nostr.relays.push(relay.clone());
        }
    }

    Ok(())
}

pub(crate) fn preferred_join_request_recipient(network: &NetworkConfig) -> Option<String> {
    if !network.invite_inviter.is_empty()
        && network
            .admins
            .iter()
            .any(|admin| admin == &network.invite_inviter)
    {
        return Some(network.invite_inviter.clone());
    }
    network.admins.first().cloned()
}

pub(crate) fn is_valid_relay_url(value: &str) -> bool {
    value.starts_with("ws://") || value.starts_with("wss://")
}

fn network_should_adopt_invite(network: &NetworkConfig) -> bool {
    let trimmed = network.name.trim();
    network.participants.is_empty() && (trimmed.is_empty() || trimmed.starts_with("Network "))
}

fn normalized_invite_pubkeys(pubkeys: &[String]) -> Result<Vec<String>> {
    let mut normalized = pubkeys
        .iter()
        .map(|pubkey| normalize_nostr_pubkey(pubkey).map(|value| to_npub(&value)))
        .collect::<Result<Vec<_>>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalized_invite_relays(relays: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for relay in relays {
        let relay = relay.trim();
        if relay.is_empty() {
            continue;
        }
        if !is_valid_relay_url(relay) {
            return Err(anyhow!("invalid invite relay '{relay}'"));
        }
        if !normalized.iter().any(|existing| existing == relay) {
            normalized.push(relay.to_string());
        }
    }
    Ok(normalized)
}
