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
    #[serde(default)]
    pub resource: Option<AgentResourceSnapshot>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentResourceSnapshot {
    pub cpu_percent: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_total_bytes: u64,
    pub network_rx_bytes: u64,
    pub network_tx_bytes: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppRuntimeHealthResponse {
    pub app_id: Uuid,
    pub configured: bool,
    pub reachable: bool,
    pub upstream_host: Option<String>,
    pub upstream_port: Option<u16>,
    pub deployment_id: Option<Uuid>,
    pub checked_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerPortMapping {
    pub container_port: u16,
    pub protocol: String,
    pub host_ip: Option<String>,
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerHealth {
    pub status: String,
    pub last_output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerSummary {
    pub id: String,
    pub name: String,
    pub service_name: Option<String>,
    pub image: String,
    pub status: String,
    pub started_at: Option<String>,
    pub restart_count: u64,
    pub port_mappings: Vec<AppContainerPortMapping>,
    pub health: Option<AppContainerHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerListResponse {
    pub app_id: Uuid,
    pub project_name: String,
    pub items: Vec<AppContainerSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerExposedPort {
    pub container_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerNetworkAttachment {
    pub network_name: String,
    pub ip_address: Option<String>,
    pub gateway: Option<String>,
    pub mac_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerMount {
    pub mount_type: String,
    pub source: Option<String>,
    pub destination: String,
    pub mode: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerEnvVar {
    pub key: String,
    pub value: String,
    pub masked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerDetails {
    pub id: String,
    pub name: String,
    pub service_name: Option<String>,
    pub image: String,
    pub status: String,
    pub started_at: Option<String>,
    pub created_at: Option<String>,
    pub restart_count: u64,
    pub command: Vec<String>,
    pub labels: std::collections::BTreeMap<String, String>,
    pub port_mappings: Vec<AppContainerPortMapping>,
    pub exposed_ports: Vec<AppContainerExposedPort>,
    pub networks: Vec<AppContainerNetworkAttachment>,
    pub mounts: Vec<AppContainerMount>,
    pub env: Vec<AppContainerEnvVar>,
    pub health: Option<AppContainerHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerDetailsResponse {
    pub app_id: Uuid,
    pub project_name: String,
    pub container: AppContainerDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContainerLogsResponse {
    pub app_id: Uuid,
    pub project_name: String,
    pub container_id: String,
    pub logs: String,
    pub next_since_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDeploymentRequest {
    pub source_ref: Option<String>,
    pub image_ref: Option<String>,
    pub commit_sha: Option<String>,
    pub simulate_failures: Option<u32>,
    pub force_rebuild: Option<bool>,
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
pub struct AppEnvVarSummary {
    pub app_id: Uuid,
    pub key: String,
    pub value: String,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEnvVarListResponse {
    pub items: Vec<AppEnvVarSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertAppEnvVarRequest {
    pub key: String,
    pub value: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashboardSummary {
    pub applications_total: u64,
    pub managed_services_healthy: u64,
    pub domains_total: u64,
    pub uptime_30d_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashboardMetricsScope {
    pub window: String,
    pub bucket: String,
    pub app_id: Option<Uuid>,
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestTrafficPoint {
    pub bucket_start_unix_ms: u64,
    pub total_requests: u64,
    pub errors_4xx: u64,
    pub errors_5xx: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerResourcePoint {
    pub bucket_start_unix_ms: u64,
    pub cpu_percent_avg: f64,
    pub memory_used_bytes_avg: u64,
    pub memory_total_bytes_avg: u64,
    pub disk_used_bytes_avg: u64,
    pub disk_total_bytes_avg: u64,
    pub network_rx_bytes_avg: u64,
    pub network_tx_bytes_avg: u64,
    pub samples: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashboardMetricsResponse {
    pub summary: DashboardSummary,
    pub scope: DashboardMetricsScope,
    pub request_traffic: Vec<RequestTrafficPoint>,
    pub server_resources: Vec<ServerResourcePoint>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn agent_resource_snapshot_json_shape_roundtrip() {
        let snapshot = AgentResourceSnapshot {
            cpu_percent: 37.5,
            memory_used_bytes: 1_500,
            memory_total_bytes: 4_000,
            disk_used_bytes: 40_000,
            disk_total_bytes: 120_000,
            network_rx_bytes: 9_000,
            network_tx_bytes: 8_500,
        };
        let encoded = serde_json::to_value(&snapshot).expect("serialize resource snapshot");
        assert_eq!(
            encoded,
            json!({
                "cpu_percent": 37.5,
                "memory_used_bytes": 1500,
                "memory_total_bytes": 4000,
                "disk_used_bytes": 40000,
                "disk_total_bytes": 120000,
                "network_rx_bytes": 9000,
                "network_tx_bytes": 8500
            })
        );
        let decoded: AgentResourceSnapshot =
            serde_json::from_value(encoded).expect("deserialize resource snapshot");
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn dashboard_metrics_response_json_shape_roundtrip() {
        let app_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let response = DashboardMetricsResponse {
            summary: DashboardSummary {
                applications_total: 2,
                managed_services_healthy: 1,
                domains_total: 3,
                uptime_30d_percent: Some(99.7),
            },
            scope: DashboardMetricsScope {
                window: "24h".to_string(),
                bucket: "5m".to_string(),
                app_id: Some(app_id),
                start_unix_ms: 1_700_000_000_000,
                end_unix_ms: 1_700_086_400_000,
            },
            request_traffic: vec![RequestTrafficPoint {
                bucket_start_unix_ms: 1_700_000_000_000,
                total_requests: 120,
                errors_4xx: 4,
                errors_5xx: 2,
            }],
            server_resources: vec![ServerResourcePoint {
                bucket_start_unix_ms: 1_700_000_000_000,
                cpu_percent_avg: 21.3,
                memory_used_bytes_avg: 2_000,
                memory_total_bytes_avg: 8_000,
                disk_used_bytes_avg: 50_000,
                disk_total_bytes_avg: 100_000,
                network_rx_bytes_avg: 10_000,
                network_tx_bytes_avg: 12_000,
                samples: 5,
            }],
        };

        let encoded = serde_json::to_value(&response).expect("serialize dashboard metrics");
        assert_eq!(
            encoded,
            json!({
                "summary": {
                    "applications_total": 2,
                    "managed_services_healthy": 1,
                    "domains_total": 3,
                    "uptime_30d_percent": 99.7
                },
                "scope": {
                    "window": "24h",
                    "bucket": "5m",
                    "app_id": "11111111-1111-1111-1111-111111111111",
                    "start_unix_ms": 1700000000000u64,
                    "end_unix_ms": 1700086400000u64
                },
                "request_traffic": [{
                    "bucket_start_unix_ms": 1700000000000u64,
                    "total_requests": 120,
                    "errors_4xx": 4,
                    "errors_5xx": 2
                }],
                "server_resources": [{
                    "bucket_start_unix_ms": 1700000000000u64,
                    "cpu_percent_avg": 21.3,
                    "memory_used_bytes_avg": 2000,
                    "memory_total_bytes_avg": 8000,
                    "disk_used_bytes_avg": 50000,
                    "disk_total_bytes_avg": 100000,
                    "network_rx_bytes_avg": 10000,
                    "network_tx_bytes_avg": 12000,
                    "samples": 5
                }]
            })
        );

        let decoded: DashboardMetricsResponse =
            serde_json::from_value(encoded).expect("deserialize dashboard metrics");
        assert_eq!(decoded, response);
    }

    #[test]
    fn app_runtime_health_response_json_shape_roundtrip() {
        let app_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let deployment_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let response = AppRuntimeHealthResponse {
            app_id,
            configured: true,
            reachable: false,
            upstream_host: Some("127.0.0.1".to_string()),
            upstream_port: Some(32000),
            deployment_id: Some(deployment_id),
            checked_at_unix_ms: 1_700_000_000_000,
        };
        let encoded = serde_json::to_value(&response).expect("serialize runtime health");
        assert_eq!(
            encoded,
            json!({
                "app_id": "22222222-2222-2222-2222-222222222222",
                "configured": true,
                "reachable": false,
                "upstream_host": "127.0.0.1",
                "upstream_port": 32000,
                "deployment_id": "33333333-3333-3333-3333-333333333333",
                "checked_at_unix_ms": 1700000000000u64
            })
        );
        let decoded: AppRuntimeHealthResponse =
            serde_json::from_value(encoded).expect("deserialize runtime health");
        assert_eq!(decoded, response);
    }

    #[test]
    fn app_container_list_response_json_shape_roundtrip() {
        let app_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let response = AppContainerListResponse {
            app_id,
            project_name: "rustploy-444444444444".to_string(),
            items: vec![AppContainerSummary {
                id: "abc123".to_string(),
                name: "web".to_string(),
                service_name: Some("web".to_string()),
                image: "nginx:1.27".to_string(),
                status: "running".to_string(),
                started_at: Some("2026-03-02T20:20:20Z".to_string()),
                restart_count: 1,
                port_mappings: vec![AppContainerPortMapping {
                    container_port: 80,
                    protocol: "tcp".to_string(),
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(32768),
                }],
                health: Some(AppContainerHealth {
                    status: "healthy".to_string(),
                    last_output: Some("ok".to_string()),
                }),
            }],
        };
        let encoded = serde_json::to_value(&response).expect("serialize container list");
        assert_eq!(
            encoded,
            json!({
                "app_id": "44444444-4444-4444-4444-444444444444",
                "project_name": "rustploy-444444444444",
                "items": [{
                    "id": "abc123",
                    "name": "web",
                    "service_name": "web",
                    "image": "nginx:1.27",
                    "status": "running",
                    "started_at": "2026-03-02T20:20:20Z",
                    "restart_count": 1,
                    "port_mappings": [{
                        "container_port": 80,
                        "protocol": "tcp",
                        "host_ip": "0.0.0.0",
                        "host_port": 32768
                    }],
                    "health": {
                        "status": "healthy",
                        "last_output": "ok"
                    }
                }]
            })
        );
        let decoded: AppContainerListResponse =
            serde_json::from_value(encoded).expect("deserialize container list");
        assert_eq!(decoded, response);
    }

    #[test]
    fn app_container_logs_response_json_shape_roundtrip() {
        let app_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let response = AppContainerLogsResponse {
            app_id,
            project_name: "rustploy-444444444444".to_string(),
            container_id: "abc123".to_string(),
            logs: "2026-03-03T00:00:00.000000000Z boot complete".to_string(),
            next_since_unix_ms: Some(1_709_440_000_123),
        };
        let encoded = serde_json::to_value(&response).expect("serialize container logs response");
        assert_eq!(
            encoded,
            json!({
                "app_id": "44444444-4444-4444-4444-444444444444",
                "project_name": "rustploy-444444444444",
                "container_id": "abc123",
                "logs": "2026-03-03T00:00:00.000000000Z boot complete",
                "next_since_unix_ms": 1709440000123u64
            })
        );
        let decoded: AppContainerLogsResponse =
            serde_json::from_value(encoded).expect("deserialize container logs response");
        assert_eq!(decoded, response);
    }
}
