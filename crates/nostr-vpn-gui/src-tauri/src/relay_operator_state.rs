use super::*;

impl NvpnBackend {
    pub(crate) fn relay_operator_state_path(&self) -> PathBuf {
        self.config_path.with_file_name("relay.operator.json")
    }

    pub(crate) fn refresh_relay_operator_state(&mut self) {
        let path = self.relay_operator_state_path();
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.relay_operator_state = None;
                return;
            }
            Err(error) => {
                eprintln!(
                    "gui: failed to read relay operator state {}: {error}",
                    path.display()
                );
                self.relay_operator_state = None;
                return;
            }
        };

        match parse_service_operator_state(&raw) {
            Ok(state) => {
                self.relay_operator_state = Some(state);
            }
            Err(error) => {
                eprintln!(
                    "gui: failed to parse relay operator state {}: {error}",
                    path.display()
                );
                self.relay_operator_state = None;
            }
        }
    }

    pub(crate) fn relay_operator_view(&self) -> Option<RelayOperatorView> {
        let state = self.relay_operator_state.as_ref()?.relay.as_ref()?;
        let now = current_unix_timestamp();
        let updated_text = compact_age_text(now.saturating_sub(state.updated_at));

        Some(RelayOperatorView {
            relay_npub: to_npub(&state.relay_pubkey),
            relay_pubkey_hex: state.relay_pubkey.clone(),
            advertised_endpoint: state.advertised_endpoint.clone(),
            total_sessions_served: state.total_sessions_served,
            total_forwarded_bytes: state.total_forwarded_bytes,
            current_forward_bps: state.current_forward_bps,
            unique_peer_count: state.unique_peer_count,
            active_session_count: state.active_sessions.len(),
            updated_text,
            active_sessions: state
                .active_sessions
                .iter()
                .map(|session| RelayOperatorSessionView {
                    request_id: session.request_id.clone(),
                    network_id: session.network_id.clone(),
                    requester_npub: to_npub(&session.requester_pubkey),
                    requester_pubkey_hex: session.requester_pubkey.clone(),
                    target_npub: to_npub(&session.target_pubkey),
                    target_pubkey_hex: session.target_pubkey.clone(),
                    requester_ingress_endpoint: session.requester_ingress_endpoint.clone(),
                    target_ingress_endpoint: session.target_ingress_endpoint.clone(),
                    started_text: compact_age_text(now.saturating_sub(session.started_at)),
                    expires_text: compact_remaining_text(session.expires_at.saturating_sub(now)),
                    bytes_from_requester: session.bytes_from_requester,
                    bytes_from_target: session.bytes_from_target,
                    total_forwarded_bytes: session
                        .bytes_from_requester
                        .saturating_add(session.bytes_from_target),
                })
                .collect(),
        })
    }

    pub(crate) fn npub_or_none(&self, value: &str) -> Option<String> {
        PublicKey::from_hex(value)
            .ok()
            .and_then(|key| key.to_bech32().ok())
    }
}
