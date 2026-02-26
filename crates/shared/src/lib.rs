use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

impl HealthResponse {
    pub fn ok() -> Self {
        Self {
            status: "ok".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    pub agent_id: Uuid,
    pub timestamp_unix_ms: u128,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatAccepted {
    pub accepted: bool,
}

impl HeartbeatAccepted {
    pub fn yes() -> Self {
        Self { accepted: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_ok_response_is_stable() {
        let payload = HealthResponse::ok();
        assert_eq!(payload.status, "ok");
    }

    #[test]
    fn heartbeat_ack_is_true() {
        let ack = HeartbeatAccepted::yes();
        assert!(ack.accepted);
    }
}
