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
    pub timestamp_unix_ms: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegisterRequest {
    pub agent_id: Uuid,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistered {
    pub accepted: bool,
    pub created: bool,
}

impl AgentRegistered {
    pub fn from_created(created: bool) -> Self {
        Self {
            accepted: true,
            created,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Online,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub agent_id: Uuid,
    pub agent_version: String,
    pub first_seen_unix_ms: u64,
    pub last_seen_unix_ms: u64,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentListResponse {
    pub items: Vec<AgentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAppRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    pub id: Uuid,
    pub name: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppListResponse {
    pub items: Vec<AppSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDeploymentRequest {
    pub source_ref: Option<String>,
    pub image_ref: Option<String>,
    pub commit_sha: Option<String>,
    pub simulate_failures: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDeploymentAccepted {
    pub deployment_id: Uuid,
    pub status: DeploymentStatus,
    pub queued_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeploymentStatus {
    Queued,
    Deploying,
    Retrying,
    Healthy,
    Failed,
}

impl DeploymentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Deploying => "deploying",
            Self::Retrying => "retrying",
            Self::Healthy => "healthy",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "deploying" => Some(Self::Deploying),
            "retrying" => Some(Self::Retrying),
            "healthy" => Some(Self::Healthy),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentSummary {
    pub id: Uuid,
    pub app_id: Uuid,
    pub source_ref: Option<String>,
    pub image_ref: Option<String>,
    pub commit_sha: Option<String>,
    pub status: DeploymentStatus,
    pub last_error: Option<String>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentListResponse {
    pub items: Vec<DeploymentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentLogsResponse {
    pub deployment_id: Uuid,
    pub logs: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubConnectRequest {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub installation_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubIntegrationSummary {
    pub app_id: Uuid,
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub installation_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubWebhookAccepted {
    pub accepted: bool,
    pub matched_integrations: u32,
    pub queued_deployments: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryRef {
    pub provider: String,
    pub owner: String,
    pub name: String,
    pub clone_url: Option<String>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub port: Option<u16>,
    pub healthcheck_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyProfile {
    pub postgres: Option<bool>,
    pub redis: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportAppRequest {
    pub repository: RepositoryRef,
    pub source: Option<SourceRef>,
    pub build_mode: Option<String>,
    pub runtime: Option<RuntimeConfig>,
    pub dependency_profile: Option<DependencyProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionResult {
    pub framework: String,
    pub package_manager: String,
    pub lockfile: Option<String>,
    pub build_profile: String,
    pub dockerfile_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeServiceSummary {
    pub name: String,
    pub image: Option<String>,
    pub build: bool,
    pub depends_on: Vec<String>,
    pub ports: Vec<String>,
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeSummary {
    pub file: String,
    pub app_service: Option<String>,
    pub services: Vec<ComposeServiceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextAction {
    pub action_type: String,
    pub deploy_endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportAppResponse {
    pub app: AppSummary,
    pub detection: DetectionResult,
    pub compose: Option<ComposeSummary>,
    pub next_action: NextAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveAppConfigResponse {
    pub app_id: Uuid,
    pub repository: RepositoryRef,
    pub source: SourceRef,
    pub build_mode: String,
    pub detection: DetectionResult,
    pub compose: Option<ComposeSummary>,
    pub dependency_profile: Option<DependencyProfile>,
    pub manifest: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorDetail {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub details: Vec<ApiErrorDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: ApiError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthLoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSessionResponse {
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    pub session_expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasswordResetRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasswordResetRequestedResponse {
    pub accepted: bool,
    pub reset_token: String,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasswordResetConfirmRequest {
    pub reset_token: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasswordResetConfirmedResponse {
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainSummary {
    pub id: Uuid,
    pub app_id: Uuid,
    pub domain: String,
    pub tls_mode: String,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDomainRequest {
    pub domain: String,
    pub tls_mode: Option<String>,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainListResponse {
    pub items: Vec<DomainSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTokenRequest {
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_in_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSummary {
    pub id: Uuid,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at_unix_ms: u64,
    pub expires_at_unix_ms: Option<u64>,
    pub revoked_at_unix_ms: Option<u64>,
    pub last_used_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenListResponse {
    pub items: Vec<TokenSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTokenResponse {
    pub token: String,
    pub summary: TokenSummary,
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

    #[test]
    fn agent_register_ack_tracks_created_state() {
        let created = AgentRegistered::from_created(true);
        let existing = AgentRegistered::from_created(false);
        assert!(created.created);
        assert!(!existing.created);
    }

    #[test]
    fn deployment_status_roundtrip() {
        assert_eq!(
            DeploymentStatus::parse("queued"),
            Some(DeploymentStatus::Queued)
        );
        assert_eq!(DeploymentStatus::Healthy.as_str(), "healthy");
        assert_eq!(DeploymentStatus::parse("unknown"), None);
    }
}
