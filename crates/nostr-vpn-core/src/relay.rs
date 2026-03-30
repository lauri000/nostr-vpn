use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayAllocationRequest {
    pub request_id: String,
    pub network_id: String,
    pub target_pubkey: String,
    pub requested_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayAllocationGranted {
    pub request_id: String,
    pub network_id: String,
    pub relay_pubkey: String,
    pub requester_ingress_endpoint: String,
    pub target_ingress_endpoint: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayProbeRequest {
    pub request_id: String,
    pub requested_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayProbeGranted {
    pub request_id: String,
    pub relay_pubkey: String,
    pub requester_ingress_endpoint: String,
    pub target_ingress_endpoint: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayAllocationRejectReason {
    OverCapacity,
    TooManySessionsForRequester,
    ByteLimitExceeded,
    RateLimited,
    InvalidRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayAllocationRejected {
    pub request_id: String,
    pub network_id: String,
    pub relay_pubkey: String,
    pub reason: RelayAllocationRejectReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayProbeRejected {
    pub request_id: String,
    pub relay_pubkey: String,
    pub reason: RelayAllocationRejectReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaySession {
    pub request_id: String,
    pub network_id: String,
    pub relay_pubkey: String,
    pub ingress_endpoint: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RelayOperatorSessionState {
    pub request_id: String,
    pub network_id: String,
    pub requester_pubkey: String,
    pub target_pubkey: String,
    pub requester_ingress_endpoint: String,
    pub target_ingress_endpoint: String,
    pub started_at: u64,
    pub expires_at: u64,
    #[serde(default)]
    pub bytes_from_requester: u64,
    #[serde(default)]
    pub bytes_from_target: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RelayOperatorState {
    pub updated_at: u64,
    pub relay_pubkey: String,
    #[serde(default)]
    pub advertised_endpoint: String,
    #[serde(default)]
    pub total_sessions_served: u64,
    #[serde(default)]
    pub total_forwarded_bytes: u64,
    #[serde(default)]
    pub current_forward_bps: u64,
    #[serde(default)]
    pub unique_peer_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_peer_pubkeys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_sessions: Vec<RelayOperatorSessionState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NatAssistOperatorState {
    pub updated_at: u64,
    #[serde(default)]
    pub advertised_endpoint: String,
    #[serde(default)]
    pub total_discovery_requests: u64,
    #[serde(default)]
    pub total_punch_requests: u64,
    #[serde(default)]
    pub current_request_bps: u64,
    #[serde(default)]
    pub unique_client_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ServiceOperatorState {
    pub updated_at: u64,
    #[serde(default)]
    pub operator_pubkey: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<RelayOperatorState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nat_assist: Option<NatAssistOperatorState>,
}
