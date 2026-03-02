use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, StatusCode},
    response::{sse::Event, sse::KeepAlive, Html, Sse},
    routing::{delete, get, post},
    Json, Router,
};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shared::{
    AgentHeartbeat, AgentListResponse, AgentRegisterRequest, AgentRegistered, AgentStatus,
    AgentSummary, ApiError, ApiErrorDetail, ApiErrorResponse, AppEnvVarListResponse,
    AppEnvVarSummary, AppListResponse, AppSummary, AuthLoginRequest, AuthSessionResponse,
    ComposeServiceSummary, ComposeSummary, CreateAppRequest, CreateDeploymentAccepted,
    CreateDeploymentRequest, CreateDomainRequest, CreateTokenRequest, CreateTokenResponse,
    DependencyProfile, DeploymentListResponse, DeploymentLogsResponse, DeploymentStatus,
    DeploymentSummary, DetectionResult, DomainListResponse, DomainSummary,
    EffectiveAppConfigResponse, GithubConnectRequest, GithubIntegrationSummary,
    GithubWebhookAccepted, HealthResponse, HeartbeatAccepted, ImportAppRequest, ImportAppResponse,
    NextAction, PasswordResetConfirmRequest, PasswordResetConfirmedResponse, PasswordResetRequest,
    PasswordResetRequestedResponse, RepositoryRef, SourceRef, TokenListResponse, TokenSummary,
    UpsertAppEnvVarRequest,
};
use tokio_stream::{wrappers::IntervalStream, StreamExt};
use tracing::{error, info, warn};
use uuid::Uuid;

const DEFAULT_ONLINE_WINDOW_MS: u64 = 30_000;
const DEFAULT_DB_PATH: &str = "data/rustploy.db";
const DEFAULT_RECONCILER_ENABLED: bool = true;
const DEFAULT_RECONCILER_INTERVAL_MS: u64 = 1_000;
const DEFAULT_JOB_BACKOFF_BASE_MS: u64 = 1_000;
const DEFAULT_JOB_BACKOFF_MAX_MS: u64 = 30_000;
const DEFAULT_JOB_MAX_ATTEMPTS: u32 = 5;
const DEFAULT_API_RATE_LIMIT_PER_MINUTE: u32 = 300;
const DEFAULT_WEBHOOK_RATE_LIMIT_PER_MINUTE: u32 = 120;
const AGENT_TOKEN_HEADER: &str = "x-rustploy-agent-token";
const GITHUB_SIGNATURE_HEADER: &str = "x-hub-signature-256";
const GITHUB_EVENT_HEADER: &str = "x-github-event";
const AUTHORIZATION_HEADER: &str = "authorization";
const COOKIE_HEADER: &str = "cookie";
const HOST_HEADER: &str = "host";
const SESSION_COOKIE_NAME: &str = "rustploy_session";
const DEFAULT_SESSION_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const DEFAULT_PASSWORD_RESET_TTL_MS: u64 = 15 * 60 * 1000;
const DEFAULT_ADMIN_EMAIL: &str = "admin@localhost";
const DEFAULT_ADMIN_PASSWORD: &str = "admin";
const DEFAULT_CADDY_UPSTREAM: &str = "127.0.0.1:8080";
const CONTROL_PLANE_HOST: &str = "localhost";
const CONTROL_PLANE_ALT_HOST: &str = "rustploy.localhost";

const SCOPE_READ: u8 = 1;
const SCOPE_DEPLOY: u8 = 1 << 1;
const SCOPE_ADMIN: u8 = 1 << 2;

#[derive(Clone, Debug)]
pub struct AppState {
    db: Database,
    agent_shared_token: Option<String>,
    github_webhook_secret: Option<String>,
    session_ttl_ms: u64,
    password_reset_ttl_ms: u64,
    online_window_ms: u64,
    reconciler_enabled: bool,
    reconciler_interval_ms: u64,
    job_backoff_base_ms: u64,
    job_backoff_max_ms: u64,
    job_max_attempts: u32,
    api_rate_limit_per_minute: u32,
    webhook_rate_limit_per_minute: u32,
    rate_limit_state: Arc<Mutex<HashMap<String, (u64, u32)>>>,
}

#[derive(Clone, Debug)]
struct AppStateConfig {
    db_path: String,
    agent_shared_token: Option<String>,
    github_webhook_secret: Option<String>,
    session_ttl_ms: u64,
    password_reset_ttl_ms: u64,
    bootstrap_admin_email: Option<String>,
    bootstrap_admin_password: Option<String>,
    online_window_ms: u64,
    reconciler_enabled: bool,
    reconciler_interval_ms: u64,
    job_backoff_base_ms: u64,
    job_backoff_max_ms: u64,
    job_max_attempts: u32,
    api_rate_limit_per_minute: u32,
    webhook_rate_limit_per_minute: u32,
}

#[derive(Clone, Debug)]
struct Database {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug)]
struct JobRecord {
    id: Uuid,
    job_type: String,
    payload_json: String,
    attempt_count: u32,
    max_attempts: u32,
}

#[derive(Debug)]
struct AuthenticatedToken {
    scope_mask: u8,
}

#[derive(Debug)]
struct AuthenticatedSession {
    user_id: Uuid,
    email: String,
    role: String,
}

#[derive(Debug, Clone, Copy)]
enum RequiredScope {
    Read,
    Deploy,
    Admin,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeployJobPayload {
    deployment_id: Uuid,
    app_id: Uuid,
    simulate_failures: u32,
    #[serde(default)]
    source_ref: Option<String>,
    #[serde(default)]
    commit_sha: Option<String>,
    #[serde(default)]
    force_rebuild: bool,
}

#[derive(Debug)]
struct ImportConfigRecord {
    app_id: Uuid,
    repository: RepositoryRef,
    source: SourceRef,
    build_mode: String,
    detection: DetectionResult,
    compose: Option<ComposeSummary>,
    dependency_profile: Option<DependencyProfile>,
    manifest_json: Option<String>,
}

#[derive(Debug)]
struct ManagedServiceRecord {
    host: String,
    port: u16,
    username: String,
    password: String,
    healthy: bool,
}

#[derive(Debug, Clone)]
struct AppRuntimeRoute {
    app_id: Uuid,
    deployment_id: Uuid,
    project_name: String,
    service_name: String,
    upstream_host: String,
    upstream_port: u16,
}

#[derive(Debug, Clone)]
struct AppEnvVarRecord {
    app_id: Uuid,
    key: String,
    value: String,
    updated_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
struct DeploymentLogRow {
    id: i64,
    message: String,
}

#[derive(Debug)]
struct MetricsSnapshot {
    apps_total: u64,
    deployments_total: u64,
    deployments_healthy: u64,
    deployments_failed: u64,
    deployments_queued: u64,
    domains_total: u64,
    tokens_total: u64,
}

#[derive(Debug)]
struct ManifestValidationError {
    field: String,
    message: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RustployManifest {
    version: Option<u32>,
    app: Option<ManifestAppConfig>,
    build: Option<ManifestBuildConfig>,
    dependencies: Option<ManifestDependencies>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestAppConfig {
    name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestBuildConfig {
    mode: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestDependencies {
    postgres: Option<ManifestDependencyToggle>,
    redis: Option<ManifestDependencyToggle>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestDependencyToggle {
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ComposeFileRaw {
    services: HashMap<String, ComposeServiceRaw>,
}

#[derive(Debug, Deserialize)]
struct ComposeServiceRaw {
    image: Option<String>,
    build: Option<serde_yaml::Value>,
    depends_on: Option<serde_yaml::Value>,
    ports: Option<Vec<serde_yaml::Value>>,
    env_file: Option<serde_yaml::Value>,
    profiles: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct GithubPushEvent {
    #[serde(rename = "ref")]
    git_ref: String,
    after: String,
    repository: GithubRepository,
}

#[derive(Debug, Deserialize)]
struct GithubRepository {
    name: String,
    owner: GithubOwner,
}

#[derive(Debug, Deserialize)]
struct GithubOwner {
    login: String,
}

impl AppState {
    pub fn from_env() -> Result<Self> {
        Self::from_config(AppStateConfig {
            db_path: std::env::var("RUSTPLOY_DB_PATH")
                .unwrap_or_else(|_| DEFAULT_DB_PATH.to_string()),
            agent_shared_token: std::env::var("RUSTPLOY_AGENT_TOKEN").ok(),
            github_webhook_secret: std::env::var("RUSTPLOY_GITHUB_WEBHOOK_SECRET").ok(),
            session_ttl_ms: read_env_u64("RUSTPLOY_SESSION_TTL_MS", DEFAULT_SESSION_TTL_MS)?,
            password_reset_ttl_ms: read_env_u64(
                "RUSTPLOY_PASSWORD_RESET_TTL_MS",
                DEFAULT_PASSWORD_RESET_TTL_MS,
            )?,
            bootstrap_admin_email: Some(
                std::env::var("RUSTPLOY_ADMIN_EMAIL")
                    .unwrap_or_else(|_| DEFAULT_ADMIN_EMAIL.to_string()),
            ),
            bootstrap_admin_password: Some(
                std::env::var("RUSTPLOY_ADMIN_PASSWORD")
                    .unwrap_or_else(|_| DEFAULT_ADMIN_PASSWORD.to_string()),
            ),
            online_window_ms: read_env_u64(
                "RUSTPLOY_AGENT_ONLINE_WINDOW_MS",
                DEFAULT_ONLINE_WINDOW_MS,
            )?,
            reconciler_enabled: read_env_bool(
                "RUSTPLOY_RECONCILER_ENABLED",
                DEFAULT_RECONCILER_ENABLED,
            ),
            reconciler_interval_ms: read_env_u64(
                "RUSTPLOY_RECONCILER_INTERVAL_MS",
                DEFAULT_RECONCILER_INTERVAL_MS,
            )?,
            job_backoff_base_ms: read_env_u64(
                "RUSTPLOY_JOB_BACKOFF_BASE_MS",
                DEFAULT_JOB_BACKOFF_BASE_MS,
            )?,
            job_backoff_max_ms: read_env_u64(
                "RUSTPLOY_JOB_BACKOFF_MAX_MS",
                DEFAULT_JOB_BACKOFF_MAX_MS,
            )?,
            job_max_attempts: read_env_u32("RUSTPLOY_JOB_MAX_ATTEMPTS", DEFAULT_JOB_MAX_ATTEMPTS)?,
            api_rate_limit_per_minute: read_env_u32(
                "RUSTPLOY_API_RATE_LIMIT_PER_MINUTE",
                DEFAULT_API_RATE_LIMIT_PER_MINUTE,
            )?,
            webhook_rate_limit_per_minute: read_env_u32(
                "RUSTPLOY_WEBHOOK_RATE_LIMIT_PER_MINUTE",
                DEFAULT_WEBHOOK_RATE_LIMIT_PER_MINUTE,
            )?,
        })
    }

    fn from_config(config: AppStateConfig) -> Result<Self> {
        let db = Database::open(&config.db_path)
            .with_context(|| format!("failed to open db at {}", config.db_path))?;

        if let (Some(email), Some(password)) = (
            config.bootstrap_admin_email.as_deref(),
            config.bootstrap_admin_password.as_deref(),
        ) {
            db.ensure_admin_user(email, password, now_unix_ms())?;
        }

        let state = Self {
            db,
            agent_shared_token: config.agent_shared_token,
            github_webhook_secret: config.github_webhook_secret,
            session_ttl_ms: config.session_ttl_ms,
            password_reset_ttl_ms: config.password_reset_ttl_ms,
            online_window_ms: config.online_window_ms,
            reconciler_enabled: config.reconciler_enabled,
            reconciler_interval_ms: config.reconciler_interval_ms,
            job_backoff_base_ms: config.job_backoff_base_ms,
            job_backoff_max_ms: config.job_backoff_max_ms,
            job_max_attempts: config.job_max_attempts,
            api_rate_limit_per_minute: config.api_rate_limit_per_minute,
            webhook_rate_limit_per_minute: config.webhook_rate_limit_per_minute,
            rate_limit_state: Arc::new(Mutex::new(HashMap::new())),
        };

        if let Err(error) = sync_caddyfile_from_db(&state.db) {
            warn!(%error, "failed syncing caddyfile during startup");
        }

        Ok(state)
    }

    pub fn reconciler_enabled(&self) -> bool {
        self.reconciler_enabled
    }

    pub fn reconciler_interval(&self) -> Duration {
        Duration::from_millis(self.reconciler_interval_ms)
    }

    pub fn run_reconciler_once(&self) -> Result<bool> {
        self.db.run_reconciler_once(
            now_unix_ms(),
            self.job_backoff_base_ms,
            self.job_backoff_max_ms,
        )
    }

    fn check_rate_limit(&self, key: &str, limit_per_minute: u32) -> Result<(), StatusCode> {
        if limit_per_minute == 0 {
            return Ok(());
        }
        let now_minute = now_unix_ms() / 60_000;
        let mut state = self
            .rate_limit_state
            .lock()
            .expect("rate limit mutex poisoned");
        let entry = state.entry(key.to_string()).or_insert((now_minute, 0));
        if entry.0 != now_minute {
            *entry = (now_minute, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        if entry.1 > limit_per_minute {
            Err(StatusCode::TOO_MANY_REQUESTS)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn for_tests(db_path: &str, agent_shared_token: Option<String>) -> Self {
        Self::from_config(AppStateConfig {
            db_path: db_path.to_string(),
            agent_shared_token,
            github_webhook_secret: None,
            session_ttl_ms: DEFAULT_SESSION_TTL_MS,
            password_reset_ttl_ms: DEFAULT_PASSWORD_RESET_TTL_MS,
            bootstrap_admin_email: None,
            bootstrap_admin_password: None,
            online_window_ms: DEFAULT_ONLINE_WINDOW_MS,
            reconciler_enabled: true,
            reconciler_interval_ms: 1,
            job_backoff_base_ms: 1,
            job_backoff_max_ms: 100,
            job_max_attempts: DEFAULT_JOB_MAX_ATTEMPTS,
            api_rate_limit_per_minute: 10_000,
            webhook_rate_limit_per_minute: 10_000,
        })
        .expect("test state should initialize")
    }

    #[cfg(test)]
    fn force_jobs_due_for_tests(&self) {
        self.db.force_jobs_due_for_tests();
    }
}

pub async fn run_reconciler_loop(state: AppState) {
    let mut ticker = tokio::time::interval(state.reconciler_interval());

    loop {
        ticker.tick().await;
        match state.run_reconciler_once() {
            Ok(true) => {}
            Ok(false) => {}
            Err(error) => warn!(%error, "reconciler tick failed"),
        }
    }
}

impl Database {
    fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create db directory {}", parent.display())
                })?;
            }
        }

        let conn = Connection::open(path)
            .with_context(|| format!("failed opening sqlite db at {path}"))?;
        initialize_connection(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn register_agent(&self, request: &AgentRegisterRequest, now_unix_ms: u64) -> Result<bool> {
        let now = to_i64(now_unix_ms)?;
        let agent_id = request.agent_id.to_string();

        let conn = self.conn.lock().expect("database mutex poisoned");

        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agents WHERE agent_id = ?1)",
                params![agent_id],
                |row| row.get(0),
            )
            .context("failed checking existing agent")?;

        if exists {
            conn.execute(
                "UPDATE agents
                 SET agent_version = ?2,
                     updated_at_unix_ms = ?3
                 WHERE agent_id = ?1",
                params![agent_id, request.agent_version, now],
            )
            .context("failed updating agent registration")?;
            Ok(false)
        } else {
            conn.execute(
                "INSERT INTO agents (
                    agent_id,
                    agent_version,
                    first_seen_unix_ms,
                    last_seen_unix_ms,
                    updated_at_unix_ms
                ) VALUES (?1, ?2, ?3, ?3, ?3)",
                params![agent_id, request.agent_version, now],
            )
            .context("failed inserting agent registration")?;
            Ok(true)
        }
    }

    fn record_heartbeat(&self, heartbeat: &AgentHeartbeat, received_unix_ms: u64) -> Result<()> {
        let received = to_i64(received_unix_ms)?;
        let reported = to_i64(heartbeat.timestamp_unix_ms)?;
        let agent_id = heartbeat.agent_id.to_string();

        let conn = self.conn.lock().expect("database mutex poisoned");

        conn.execute(
            "INSERT INTO agents (
                agent_id,
                agent_version,
                first_seen_unix_ms,
                last_seen_unix_ms,
                updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?3, ?3)
             ON CONFLICT(agent_id) DO UPDATE SET
                agent_version = excluded.agent_version,
                last_seen_unix_ms = excluded.last_seen_unix_ms,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![agent_id, heartbeat.agent_version, received],
        )
        .context("failed upserting agent during heartbeat")?;

        conn.execute(
            "INSERT INTO agent_heartbeats (
                agent_id,
                reported_unix_ms,
                received_unix_ms,
                agent_version
             ) VALUES (?1, ?2, ?3, ?4)",
            params![agent_id, reported, received, heartbeat.agent_version],
        )
        .context("failed inserting heartbeat event")?;

        Ok(())
    }

    fn list_agents(&self, now_unix_ms: u64, online_window_ms: u64) -> Result<Vec<AgentSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");

        let mut stmt = conn
            .prepare(
                "SELECT agent_id, agent_version, first_seen_unix_ms, last_seen_unix_ms
                 FROM agents
                 ORDER BY last_seen_unix_ms DESC",
            )
            .context("failed preparing agent listing query")?;

        let mut rows = stmt.query([]).context("failed querying agents")?;
        let mut items = Vec::new();

        while let Some(row) = rows.next().context("failed iterating agents")? {
            let agent_id_raw: String = row.get(0).context("failed reading agent_id")?;
            let agent_version: String = row.get(1).context("failed reading agent_version")?;
            let first_seen_raw: i64 = row.get(2).context("failed reading first_seen_unix_ms")?;
            let last_seen_raw: i64 = row.get(3).context("failed reading last_seen_unix_ms")?;

            let agent_id = Uuid::parse_str(&agent_id_raw)
                .with_context(|| format!("invalid agent id in db: {agent_id_raw}"))?;
            let first_seen_unix_ms = to_u64(first_seen_raw)?;
            let last_seen_unix_ms = to_u64(last_seen_raw)?;
            let delta = now_unix_ms.saturating_sub(last_seen_unix_ms);

            let status = if delta <= online_window_ms {
                AgentStatus::Online
            } else {
                AgentStatus::Offline
            };

            items.push(AgentSummary {
                agent_id,
                agent_version,
                first_seen_unix_ms,
                last_seen_unix_ms,
                status,
            });
        }

        Ok(items)
    }

    fn create_app(&self, name: &str, now_unix_ms: u64) -> Result<Option<AppSummary>> {
        let now = to_i64(now_unix_ms)?;

        let conn = self.conn.lock().expect("database mutex poisoned");

        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM apps WHERE name = ?1)",
                params![name],
                |row| row.get(0),
            )
            .context("failed checking app name uniqueness")?;

        if exists {
            return Ok(None);
        }

        let app_id = Uuid::new_v4();

        conn.execute(
            "INSERT INTO apps (id, name, created_at_unix_ms, updated_at_unix_ms)
             VALUES (?1, ?2, ?3, ?3)",
            params![app_id.to_string(), name, now],
        )
        .context("failed inserting app")?;

        Ok(Some(AppSummary {
            id: app_id,
            name: name.to_string(),
            created_at_unix_ms: now_unix_ms,
            updated_at_unix_ms: now_unix_ms,
        }))
    }

    fn list_apps(&self) -> Result<Vec<AppSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, name, created_at_unix_ms, updated_at_unix_ms
                 FROM apps
                 ORDER BY created_at_unix_ms DESC",
            )
            .context("failed preparing app listing query")?;

        let mut rows = stmt.query([]).context("failed querying apps")?;
        let mut items = Vec::new();

        while let Some(row) = rows.next().context("failed iterating apps")? {
            let app_id_raw: String = row.get(0).context("failed reading app id")?;
            let name: String = row.get(1).context("failed reading app name")?;
            let created_raw: i64 = row.get(2).context("failed reading app created time")?;
            let updated_raw: i64 = row.get(3).context("failed reading app updated time")?;

            let id = Uuid::parse_str(&app_id_raw)
                .with_context(|| format!("invalid app id in db: {app_id_raw}"))?;

            items.push(AppSummary {
                id,
                name,
                created_at_unix_ms: to_u64(created_raw)?,
                updated_at_unix_ms: to_u64(updated_raw)?,
            });
        }

        Ok(items)
    }

    fn find_app_by_name(&self, name: &str) -> Result<Option<AppSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT id, name, created_at_unix_ms, updated_at_unix_ms
                 FROM apps
                 WHERE lower(name) = lower(?1)
                 LIMIT 1",
                params![name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .context("failed querying app by name")?;

        let Some((app_id_raw, app_name, created_raw, updated_raw)) = row else {
            return Ok(None);
        };

        Ok(Some(AppSummary {
            id: Uuid::parse_str(&app_id_raw)
                .with_context(|| format!("invalid app id in db: {app_id_raw}"))?,
            name: app_name,
            created_at_unix_ms: to_u64(created_raw)?,
            updated_at_unix_ms: to_u64(updated_raw)?,
        }))
    }

    fn find_app_by_domain(&self, domain: &str) -> Result<Option<AppSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT a.id, a.name, a.created_at_unix_ms, a.updated_at_unix_ms
                 FROM domains d
                 JOIN apps a ON a.id = d.app_id
                 WHERE lower(d.domain) = lower(?1)
                 LIMIT 1",
                params![domain],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .context("failed querying app by domain")?;

        let Some((app_id_raw, app_name, created_raw, updated_raw)) = row else {
            return Ok(None);
        };

        Ok(Some(AppSummary {
            id: Uuid::parse_str(&app_id_raw)
                .with_context(|| format!("invalid app id in db: {app_id_raw}"))?,
            name: app_name,
            created_at_unix_ms: to_u64(created_raw)?,
            updated_at_unix_ms: to_u64(updated_raw)?,
        }))
    }

    fn app_exists(&self, app_id: Uuid) -> Result<bool> {
        let conn = self.conn.lock().expect("database mutex poisoned");

        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM apps WHERE id = ?1)",
            params![app_id.to_string()],
            |row| row.get(0),
        )
        .context("failed checking app existence")
    }

    fn deployment_belongs_to_app(&self, app_id: Uuid, deployment_id: Uuid) -> Result<bool> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM deployments
                WHERE id = ?1 AND app_id = ?2
            )",
            params![deployment_id.to_string(), app_id.to_string()],
            |row| row.get(0),
        )
        .context("failed checking deployment ownership")
    }

    fn upsert_import_config(&self, config: &ImportConfigRecord, now_unix_ms: u64) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let now = to_i64(now_unix_ms)?;
        let dependency_profile_json = config
            .dependency_profile
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed serializing dependency profile")?;
        let compose_json = config
            .compose
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed serializing compose summary")?;

        conn.execute(
            "INSERT INTO app_import_configs (
                app_id,
                repo_provider,
                repo_owner,
                repo_name,
                repo_clone_url,
                repo_branch,
                build_mode,
                package_manager,
                framework,
                lockfile,
                build_profile,
                dockerfile_present,
                compose_json,
                dependency_profile_json,
                manifest_json,
                created_at_unix_ms,
                updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)
             ON CONFLICT(app_id) DO UPDATE SET
                repo_provider = excluded.repo_provider,
                repo_owner = excluded.repo_owner,
                repo_name = excluded.repo_name,
                repo_clone_url = excluded.repo_clone_url,
                repo_branch = excluded.repo_branch,
                build_mode = excluded.build_mode,
                package_manager = excluded.package_manager,
                framework = excluded.framework,
                lockfile = excluded.lockfile,
                build_profile = excluded.build_profile,
                dockerfile_present = excluded.dockerfile_present,
                compose_json = excluded.compose_json,
                dependency_profile_json = excluded.dependency_profile_json,
                manifest_json = excluded.manifest_json,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                config.app_id.to_string(),
                &config.repository.provider,
                &config.repository.owner,
                &config.repository.name,
                config.repository.clone_url.as_deref(),
                config.source.branch.as_deref(),
                &config.build_mode,
                &config.detection.package_manager,
                &config.detection.framework,
                config.detection.lockfile.as_deref(),
                &config.detection.build_profile,
                if config.detection.dockerfile_present {
                    1i64
                } else {
                    0i64
                },
                compose_json,
                dependency_profile_json,
                config.manifest_json.as_deref(),
                now
            ],
        )
        .context("failed upserting import config")?;

        Ok(())
    }

    fn get_import_config(&self, app_id: Uuid) -> Result<Option<EffectiveAppConfigResponse>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT
                    app_id,
                    repo_provider,
                    repo_owner,
                    repo_name,
                    repo_clone_url,
                    repo_branch,
                    build_mode,
                    package_manager,
                    framework,
                    lockfile,
                    build_profile,
                    dockerfile_present,
                    compose_json,
                    dependency_profile_json,
                    manifest_json
                 FROM app_import_configs
                 WHERE app_id = ?1",
                params![app_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                    ))
                },
            )
            .optional()
            .context("failed querying import config")?;

        let Some((
            app_id_raw,
            repo_provider,
            repo_owner,
            repo_name,
            repo_clone_url,
            repo_branch,
            build_mode,
            package_manager,
            framework,
            lockfile,
            build_profile,
            dockerfile_present,
            compose_raw,
            dependency_profile_raw,
            manifest_raw,
        )) = row
        else {
            return Ok(None);
        };

        let compose = compose_raw
            .as_deref()
            .map(serde_json::from_str::<ComposeSummary>)
            .transpose()
            .context("failed parsing compose summary json")?;
        let dependency_profile = dependency_profile_raw
            .as_deref()
            .map(serde_json::from_str::<DependencyProfile>)
            .transpose()
            .context("failed parsing dependency profile json")?;
        let manifest = manifest_raw
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .context("failed parsing manifest json")?;

        Ok(Some(EffectiveAppConfigResponse {
            app_id: Uuid::parse_str(&app_id_raw)
                .with_context(|| format!("invalid app id in import config: {app_id_raw}"))?,
            repository: RepositoryRef {
                provider: repo_provider,
                owner: repo_owner,
                name: repo_name,
                clone_url: repo_clone_url,
                default_branch: Some(repo_branch.clone()),
            },
            source: SourceRef {
                branch: Some(repo_branch),
                commit_sha: None,
            },
            build_mode,
            detection: DetectionResult {
                framework,
                package_manager,
                lockfile,
                build_profile,
                dockerfile_present: dockerfile_present == 1,
            },
            compose,
            dependency_profile,
            manifest,
        }))
    }

    fn upsert_app_runtime(&self, route: &AppRuntimeRoute, now_unix_ms: u64) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let now = to_i64(now_unix_ms)?;
        conn.execute(
            "INSERT INTO app_runtimes (
                app_id,
                deployment_id,
                project_name,
                service_name,
                upstream_host,
                upstream_port,
                created_at_unix_ms,
                updated_at_unix_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(app_id) DO UPDATE SET
                deployment_id = excluded.deployment_id,
                project_name = excluded.project_name,
                service_name = excluded.service_name,
                upstream_host = excluded.upstream_host,
                upstream_port = excluded.upstream_port,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                route.app_id.to_string(),
                route.deployment_id.to_string(),
                route.project_name,
                route.service_name,
                route.upstream_host,
                i64::from(route.upstream_port),
                now,
            ],
        )
        .context("failed upserting app runtime route")?;
        Ok(())
    }

    fn list_app_runtimes(&self) -> Result<Vec<AppRuntimeRoute>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT app_id, deployment_id, project_name, service_name, upstream_host, upstream_port
                 FROM app_runtimes",
            )
            .context("failed preparing app runtime query")?;
        let mut rows = stmt.query([]).context("failed querying app runtimes")?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().context("failed iterating app runtimes")? {
            let app_id_raw: String = row.get(0).context("failed reading app runtime app_id")?;
            let deployment_id_raw: String = row
                .get(1)
                .context("failed reading app runtime deployment_id")?;
            let project_name: String = row
                .get(2)
                .context("failed reading app runtime project_name")?;
            let service_name: String = row
                .get(3)
                .context("failed reading app runtime service_name")?;
            let upstream_host: String = row
                .get(4)
                .context("failed reading app runtime upstream_host")?;
            let upstream_port_raw: i64 = row
                .get(5)
                .context("failed reading app runtime upstream_port")?;
            items.push(AppRuntimeRoute {
                app_id: Uuid::parse_str(&app_id_raw)
                    .with_context(|| format!("invalid app runtime app_id: {app_id_raw}"))?,
                deployment_id: Uuid::parse_str(&deployment_id_raw).with_context(|| {
                    format!("invalid app runtime deployment_id: {deployment_id_raw}")
                })?,
                project_name,
                service_name,
                upstream_host,
                upstream_port: to_u16(upstream_port_raw)?,
            });
        }
        Ok(items)
    }

    fn upsert_app_env_var(
        &self,
        app_id: Uuid,
        key: &str,
        value: &str,
        now_unix_ms: u64,
    ) -> Result<AppEnvVarRecord> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let now = to_i64(now_unix_ms)?;
        conn.execute(
            "INSERT INTO app_env_vars (
                app_id,
                key,
                value,
                created_at_unix_ms,
                updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(app_id, key) DO UPDATE SET
                value = excluded.value,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![app_id.to_string(), key, value, now],
        )
        .context("failed upserting app env var")?;

        conn.execute(
            "UPDATE apps
             SET updated_at_unix_ms = ?2
             WHERE id = ?1",
            params![app_id.to_string(), now],
        )
        .context("failed updating app timestamp after env var upsert")?;

        Ok(AppEnvVarRecord {
            app_id,
            key: key.to_string(),
            value: value.to_string(),
            updated_at_unix_ms: now_unix_ms,
        })
    }

    fn list_app_env_vars(&self, app_id: Uuid) -> Result<Vec<AppEnvVarRecord>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT app_id, key, value, updated_at_unix_ms
                 FROM app_env_vars
                 WHERE app_id = ?1
                 ORDER BY key ASC",
            )
            .context("failed preparing app env var query")?;
        let mut rows = stmt
            .query(params![app_id.to_string()])
            .context("failed querying app env vars")?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().context("failed iterating app env vars")? {
            let app_id_raw: String = row.get(0).context("failed reading env var app_id")?;
            let key: String = row.get(1).context("failed reading env var key")?;
            let value: String = row.get(2).context("failed reading env var value")?;
            let updated_raw: i64 = row
                .get(3)
                .context("failed reading env var updated_at_unix_ms")?;
            items.push(AppEnvVarRecord {
                app_id: Uuid::parse_str(&app_id_raw)
                    .with_context(|| format!("invalid app env var app_id: {app_id_raw}"))?,
                key,
                value,
                updated_at_unix_ms: to_u64(updated_raw)?,
            });
        }
        Ok(items)
    }

    fn delete_app_env_var(&self, app_id: Uuid, key: &str, now_unix_ms: u64) -> Result<bool> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let deleted = conn
            .execute(
                "DELETE FROM app_env_vars WHERE app_id = ?1 AND key = ?2",
                params![app_id.to_string(), key],
            )
            .context("failed deleting app env var")?;
        if deleted > 0 {
            conn.execute(
                "UPDATE apps
                 SET updated_at_unix_ms = ?2
                 WHERE id = ?1",
                params![app_id.to_string(), to_i64(now_unix_ms)?],
            )
            .context("failed updating app timestamp after env var delete")?;
        }
        Ok(deleted > 0)
    }

    fn latest_healthy_source_ref(&self, app_id: Uuid) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.query_row(
            "SELECT source_ref
             FROM deployments
             WHERE app_id = ?1
               AND status = ?2
               AND source_ref IS NOT NULL
             ORDER BY created_at_unix_ms DESC
             LIMIT 1",
            params![app_id.to_string(), DeploymentStatus::Healthy.as_str()],
            |row| row.get(0),
        )
        .optional()
        .context("failed fetching latest healthy deployment source")
    }

    fn has_unrevoked_tokens(&self) -> Result<bool> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM api_tokens
                WHERE revoked_at_unix_ms IS NULL
            )",
            [],
            |row| row.get(0),
        )
        .context("failed checking token bootstrap state")
    }

    fn has_users(&self) -> Result<bool> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |row| row.get(0))
            .context("failed checking user bootstrap state")
    }

    fn ensure_admin_user(&self, email: &str, password: &str, now_unix_ms: u64) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM users WHERE email = ?1)",
                params![email],
                |row| row.get(0),
            )
            .context("failed checking admin existence")?;
        if exists {
            return Ok(());
        }

        let now = to_i64(now_unix_ms)?;
        conn.execute(
            "INSERT INTO users (id, email, password_hash, role, created_at_unix_ms, updated_at_unix_ms)
             VALUES (?1, ?2, ?3, 'owner', ?4, ?4)",
            params![Uuid::new_v4().to_string(), email, token_hash(password), now],
        )
        .context("failed creating bootstrap admin user")?;
        Ok(())
    }

    fn find_user_by_email(&self, email: &str) -> Result<Option<(Uuid, String, String)>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT id, password_hash, role FROM users WHERE email = ?1",
                params![email],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .context("failed querying user by email")?;

        let Some((id_raw, password_hash, role)) = row else {
            return Ok(None);
        };

        Ok(Some((
            Uuid::parse_str(&id_raw).with_context(|| format!("invalid user id in db: {id_raw}"))?,
            password_hash,
            role,
        )))
    }

    fn create_session(
        &self,
        user_id: Uuid,
        session_token_hash: &str,
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO sessions (
                id, user_id, session_token_hash, expires_at_unix_ms, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Uuid::new_v4().to_string(),
                user_id.to_string(),
                session_token_hash,
                to_i64(expires_at_unix_ms)?,
                to_i64(now_unix_ms)?,
            ],
        )
        .context("failed creating auth session")?;
        Ok(())
    }

    fn authenticate_session(
        &self,
        session_token_hash: &str,
        now_unix_ms: u64,
    ) -> Result<Option<AuthenticatedSession>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT users.id, users.email, users.role
                 FROM sessions
                 INNER JOIN users ON users.id = sessions.user_id
                 WHERE sessions.session_token_hash = ?1
                   AND sessions.revoked_at_unix_ms IS NULL
                   AND sessions.expires_at_unix_ms > ?2
                 LIMIT 1",
                params![session_token_hash, to_i64(now_unix_ms)?],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .context("failed authenticating session")?;

        let Some((user_id_raw, email, role)) = row else {
            return Ok(None);
        };

        Ok(Some(AuthenticatedSession {
            user_id: Uuid::parse_str(&user_id_raw)
                .with_context(|| format!("invalid session user id in db: {user_id_raw}"))?,
            email,
            role,
        }))
    }

    fn revoke_session(&self, session_token_hash: &str, now_unix_ms: u64) -> Result<bool> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let updated = conn
            .execute(
                "UPDATE sessions
                 SET revoked_at_unix_ms = ?2
                 WHERE session_token_hash = ?1 AND revoked_at_unix_ms IS NULL",
                params![session_token_hash, to_i64(now_unix_ms)?],
            )
            .context("failed revoking session")?;
        Ok(updated > 0)
    }

    fn issue_password_reset(
        &self,
        email: &str,
        reset_token_hash: &str,
        expires_at_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<Option<Uuid>> {
        let Some((user_id, _, _)) = self.find_user_by_email(email)? else {
            return Ok(None);
        };
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO password_reset_tokens (
                id, user_id, token_hash, expires_at_unix_ms, created_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Uuid::new_v4().to_string(),
                user_id.to_string(),
                reset_token_hash,
                to_i64(expires_at_unix_ms)?,
                to_i64(now_unix_ms)?
            ],
        )
        .context("failed issuing password reset token")?;
        Ok(Some(user_id))
    }

    fn confirm_password_reset(
        &self,
        reset_token_hash: &str,
        new_password_hash: &str,
        now_unix_ms: u64,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().expect("database mutex poisoned");
        let tx = conn
            .transaction()
            .context("failed opening password reset transaction")?;
        let row = tx
            .query_row(
                "SELECT id, user_id
                 FROM password_reset_tokens
                 WHERE token_hash = ?1
                   AND used_at_unix_ms IS NULL
                   AND expires_at_unix_ms > ?2
                 LIMIT 1",
                params![reset_token_hash, to_i64(now_unix_ms)?],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .context("failed looking up password reset token")?;
        let Some((token_id_raw, user_id_raw)) = row else {
            tx.commit()
                .context("failed committing empty password reset transaction")?;
            return Ok(false);
        };

        tx.execute(
            "UPDATE users SET password_hash = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
            params![user_id_raw, new_password_hash, to_i64(now_unix_ms)?],
        )
        .context("failed updating user password")?;
        tx.execute(
            "UPDATE password_reset_tokens SET used_at_unix_ms = ?2 WHERE id = ?1",
            params![token_id_raw, to_i64(now_unix_ms)?],
        )
        .context("failed consuming password reset token")?;
        tx.execute(
            "UPDATE sessions
             SET revoked_at_unix_ms = ?2
             WHERE user_id = ?1 AND revoked_at_unix_ms IS NULL",
            params![user_id_raw, to_i64(now_unix_ms)?],
        )
        .context("failed revoking existing sessions after password reset")?;
        tx.commit()
            .context("failed committing password reset transaction")?;
        Ok(true)
    }

    fn create_domain(
        &self,
        app_id: Uuid,
        request: &CreateDomainRequest,
        now_unix_ms: u64,
    ) -> Result<Option<DomainSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let now = to_i64(now_unix_ms)?;
        let domain = request.domain.trim().to_string();
        if domain.is_empty() {
            return Ok(None);
        }

        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM domains WHERE domain = ?1)",
                params![domain],
                |row| row.get(0),
            )
            .context("failed checking domain uniqueness")?;
        if exists {
            return Ok(None);
        }

        let tls_mode = request.tls_mode.as_deref().unwrap_or("managed").to_string();
        let domain_id = Uuid::new_v4();
        conn.execute(
            "INSERT INTO domains (
                id, app_id, domain, tls_mode, cert_path, key_path, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                domain_id.to_string(),
                app_id.to_string(),
                domain,
                tls_mode,
                request.cert_path,
                request.key_path,
                now,
            ],
        )
        .context("failed inserting domain mapping")?;

        Ok(Some(DomainSummary {
            id: domain_id,
            app_id,
            domain: request.domain.trim().to_string(),
            tls_mode,
            cert_path: request.cert_path.clone(),
            key_path: request.key_path.clone(),
            created_at_unix_ms: now_unix_ms,
        }))
    }

    fn list_domains(&self, app_id: Uuid) -> Result<Vec<DomainSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, app_id, domain, tls_mode, cert_path, key_path, created_at_unix_ms
                 FROM domains
                 WHERE app_id = ?1
                 ORDER BY created_at_unix_ms DESC",
            )
            .context("failed preparing domains query")?;
        let mut rows = stmt
            .query(params![app_id.to_string()])
            .context("failed querying domains")?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().context("failed iterating domains")? {
            let domain_id_raw: String = row.get(0).context("failed reading domain id")?;
            let app_id_raw: String = row.get(1).context("failed reading domain app id")?;
            let domain: String = row.get(2).context("failed reading domain name")?;
            let tls_mode: String = row.get(3).context("failed reading tls_mode")?;
            let cert_path: Option<String> = row.get(4).context("failed reading cert path")?;
            let key_path: Option<String> = row.get(5).context("failed reading key path")?;
            let created_raw: i64 = row.get(6).context("failed reading domain created time")?;

            items.push(DomainSummary {
                id: Uuid::parse_str(&domain_id_raw)
                    .with_context(|| format!("invalid domain id in db: {domain_id_raw}"))?,
                app_id: Uuid::parse_str(&app_id_raw)
                    .with_context(|| format!("invalid domain app id in db: {app_id_raw}"))?,
                domain,
                tls_mode,
                cert_path,
                key_path,
                created_at_unix_ms: to_u64(created_raw)?,
            });
        }
        Ok(items)
    }

    fn list_all_domains(&self) -> Result<Vec<DomainSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, app_id, domain, tls_mode, cert_path, key_path, created_at_unix_ms
                 FROM domains
                 ORDER BY created_at_unix_ms ASC",
            )
            .context("failed preparing all domains query")?;
        let mut rows = stmt.query([]).context("failed querying all domains")?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().context("failed iterating all domains")? {
            let domain_id_raw: String = row.get(0).context("failed reading domain id")?;
            let app_id_raw: String = row.get(1).context("failed reading domain app id")?;
            let domain: String = row.get(2).context("failed reading domain name")?;
            let tls_mode: String = row.get(3).context("failed reading tls_mode")?;
            let cert_path: Option<String> = row.get(4).context("failed reading cert path")?;
            let key_path: Option<String> = row.get(5).context("failed reading key path")?;
            let created_raw: i64 = row.get(6).context("failed reading domain created time")?;

            items.push(DomainSummary {
                id: Uuid::parse_str(&domain_id_raw)
                    .with_context(|| format!("invalid domain id in db: {domain_id_raw}"))?,
                app_id: Uuid::parse_str(&app_id_raw)
                    .with_context(|| format!("invalid domain app id in db: {app_id_raw}"))?,
                domain,
                tls_mode,
                cert_path,
                key_path,
                created_at_unix_ms: to_u64(created_raw)?,
            });
        }
        Ok(items)
    }

    fn metrics_snapshot(&self) -> Result<MetricsSnapshot> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let count = |sql: &str| -> Result<u64> {
            let value: i64 = conn
                .query_row(sql, [], |row| row.get(0))
                .with_context(|| format!("failed metrics query: {sql}"))?;
            to_u64(value)
        };

        Ok(MetricsSnapshot {
            apps_total: count("SELECT COUNT(*) FROM apps")?,
            deployments_total: count("SELECT COUNT(*) FROM deployments")?,
            deployments_healthy: count(
                "SELECT COUNT(*) FROM deployments WHERE status = 'healthy'",
            )?,
            deployments_failed: count(
                "SELECT COUNT(*) FROM deployments WHERE status = 'failed'",
            )?,
            deployments_queued: count(
                "SELECT COUNT(*) FROM deployments WHERE status IN ('queued','deploying','retrying')",
            )?,
            domains_total: count("SELECT COUNT(*) FROM domains")?,
            tokens_total: count("SELECT COUNT(*) FROM api_tokens WHERE revoked_at_unix_ms IS NULL")?,
        })
    }

    fn ensure_managed_service(
        &self,
        app_id: Uuid,
        service_type: &str,
        now_unix_ms: u64,
    ) -> Result<ManagedServiceRecord> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT service_type, host, port, username, password, healthy
                 FROM managed_services
                 WHERE app_id = ?1 AND service_type = ?2",
                params![app_id.to_string(), service_type],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()
            .context("failed reading managed service")?;
        if let Some((_service_type, host, port_raw, username, password, healthy_raw)) = row {
            return Ok(ManagedServiceRecord {
                host,
                port: u16::try_from(port_raw).context("managed service port out of range")?,
                username,
                password,
                healthy: healthy_raw == 1,
            });
        }

        let (host, port) = match service_type {
            "postgres" => (format!("postgres-{}.internal", app_id.simple()), 5432u16),
            "redis" => (format!("redis-{}.internal", app_id.simple()), 6379u16),
            _ => anyhow::bail!("unsupported managed service type: {service_type}"),
        };
        let username = format!("rp_{}", &Uuid::new_v4().simple().to_string()[..8]);
        let password = format!("rp_{}", Uuid::new_v4().simple());
        conn.execute(
            "INSERT INTO managed_services (
                id, app_id, service_type, username, password, host, port, healthy, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)",
            params![
                Uuid::new_v4().to_string(),
                app_id.to_string(),
                service_type,
                username,
                password,
                host,
                i64::from(port),
                to_i64(now_unix_ms)?,
            ],
        )
        .context("failed creating managed service")?;
        Ok(ManagedServiceRecord {
            host,
            port,
            username,
            password,
            healthy: true,
        })
    }

    fn create_api_token(
        &self,
        name: &str,
        token_hash: &str,
        scope_mask: u8,
        expires_at_unix_ms: Option<u64>,
        now_unix_ms: u64,
    ) -> Result<TokenSummary> {
        let token_id = Uuid::new_v4();
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO api_tokens (
                id,
                name,
                token_hash,
                scope_mask,
                created_at_unix_ms,
                expires_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                token_id.to_string(),
                name,
                token_hash,
                i64::from(scope_mask),
                to_i64(now_unix_ms)?,
                expires_at_unix_ms.map(to_i64).transpose()?
            ],
        )
        .context("failed inserting api token")?;

        Ok(TokenSummary {
            id: token_id,
            name: name.to_string(),
            scopes: scopes_from_mask(scope_mask),
            created_at_unix_ms: now_unix_ms,
            expires_at_unix_ms,
            revoked_at_unix_ms: None,
            last_used_at_unix_ms: None,
        })
    }

    fn list_api_tokens(&self) -> Result<Vec<TokenSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, name, scope_mask, created_at_unix_ms, expires_at_unix_ms, revoked_at_unix_ms, last_used_at_unix_ms
                 FROM api_tokens
                 ORDER BY created_at_unix_ms DESC",
            )
            .context("failed preparing token list query")?;
        let mut rows = stmt.query([]).context("failed querying api tokens")?;
        let mut items = Vec::new();

        while let Some(row) = rows.next().context("failed iterating api tokens")? {
            let token_id_raw: String = row.get(0).context("failed reading token id")?;
            let name: String = row.get(1).context("failed reading token name")?;
            let scope_mask_raw: i64 = row.get(2).context("failed reading token scopes")?;
            let created_raw: i64 = row.get(3).context("failed reading token created time")?;
            let expires_raw: Option<i64> = row.get(4).context("failed reading token expiry")?;
            let revoked_raw: Option<i64> =
                row.get(5).context("failed reading token revoke time")?;
            let last_used_raw: Option<i64> =
                row.get(6).context("failed reading token last_used time")?;

            let scope_mask = u8::try_from(scope_mask_raw).context("scope mask out of range")?;
            items.push(TokenSummary {
                id: Uuid::parse_str(&token_id_raw)
                    .with_context(|| format!("invalid token id in db: {token_id_raw}"))?,
                name,
                scopes: scopes_from_mask(scope_mask),
                created_at_unix_ms: to_u64(created_raw)?,
                expires_at_unix_ms: expires_raw.map(to_u64).transpose()?,
                revoked_at_unix_ms: revoked_raw.map(to_u64).transpose()?,
                last_used_at_unix_ms: last_used_raw.map(to_u64).transpose()?,
            });
        }

        Ok(items)
    }

    fn revoke_api_token(&self, token_id: Uuid, now_unix_ms: u64) -> Result<bool> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let updated = conn
            .execute(
                "UPDATE api_tokens
                 SET revoked_at_unix_ms = ?2
                 WHERE id = ?1 AND revoked_at_unix_ms IS NULL",
                params![token_id.to_string(), to_i64(now_unix_ms)?],
            )
            .context("failed revoking api token")?;
        Ok(updated > 0)
    }

    fn authenticate_api_token(
        &self,
        token_hash: &str,
        now_unix_ms: u64,
        method: &str,
        path: &str,
    ) -> Result<Option<AuthenticatedToken>> {
        let mut conn = self.conn.lock().expect("database mutex poisoned");
        let tx = conn
            .transaction()
            .context("failed opening auth transaction")?;

        let row = tx
            .query_row(
                "SELECT id, scope_mask, expires_at_unix_ms, revoked_at_unix_ms
                 FROM api_tokens
                 WHERE token_hash = ?1
                 LIMIT 1",
                params![token_hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()
            .context("failed querying api token hash")?;

        let Some((token_id_raw, scope_mask_raw, expires_at_raw, revoked_at_raw)) = row else {
            tx.commit().context("failed committing auth transaction")?;
            return Ok(None);
        };

        if revoked_at_raw.is_some() {
            tx.commit().context("failed committing auth transaction")?;
            return Ok(None);
        }

        if let Some(expires_raw) = expires_at_raw {
            if expires_raw <= to_i64(now_unix_ms)? {
                tx.commit().context("failed committing auth transaction")?;
                return Ok(None);
            }
        }

        let token_id = Uuid::parse_str(&token_id_raw)
            .with_context(|| format!("invalid token id in db: {token_id_raw}"))?;
        let scope_mask = u8::try_from(scope_mask_raw).context("scope mask out of range")?;

        tx.execute(
            "UPDATE api_tokens
             SET last_used_at_unix_ms = ?2
             WHERE id = ?1",
            params![token_id.to_string(), to_i64(now_unix_ms)?],
        )
        .context("failed updating token last_used_at")?;

        tx.execute(
            "INSERT INTO token_audit_events (token_id, method, path, created_at_unix_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![token_id.to_string(), method, path, to_i64(now_unix_ms)?],
        )
        .context("failed inserting token audit event")?;

        tx.commit().context("failed committing auth transaction")?;

        Ok(Some(AuthenticatedToken { scope_mask }))
    }

    fn upsert_github_integration(
        &self,
        app_id: Uuid,
        request: &GithubConnectRequest,
        now_unix_ms: u64,
    ) -> Result<GithubIntegrationSummary> {
        let now = to_i64(now_unix_ms)?;
        let app_id_raw = app_id.to_string();
        let install_id = request.installation_id.map(i64::try_from).transpose()?;

        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO github_integrations (
                id, app_id, owner, repo, branch, installation_id, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(app_id) DO UPDATE SET
                owner = excluded.owner,
                repo = excluded.repo,
                branch = excluded.branch,
                installation_id = excluded.installation_id,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                Uuid::new_v4().to_string(),
                app_id_raw,
                request.owner,
                request.repo,
                request.branch,
                install_id,
                now,
            ],
        )
        .context("failed upserting github integration")?;

        let row = conn
            .query_row(
                "SELECT app_id, owner, repo, branch, installation_id
                 FROM github_integrations
                 WHERE app_id = ?1",
                params![app_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .context("failed reading github integration")?;

        Ok(GithubIntegrationSummary {
            app_id: Uuid::parse_str(&row.0)
                .with_context(|| format!("invalid app id in github integration: {}", row.0))?,
            owner: row.1,
            repo: row.2,
            branch: row.3,
            installation_id: row
                .4
                .map(to_u64)
                .transpose()
                .context("invalid installation_id value")?,
        })
    }

    fn list_github_integrations_for_push(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<GithubIntegrationSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT app_id, owner, repo, branch, installation_id
                 FROM github_integrations
                 WHERE owner = ?1 AND repo = ?2 AND branch = ?3",
            )
            .context("failed preparing github integration lookup")?;

        let mut rows = stmt
            .query(params![owner, repo, branch])
            .context("failed querying github integrations")?;
        let mut items = Vec::new();

        while let Some(row) = rows
            .next()
            .context("failed iterating github integrations")?
        {
            let app_id_raw: String = row.get(0).context("failed reading integration app_id")?;
            let owner: String = row.get(1).context("failed reading integration owner")?;
            let repo: String = row.get(2).context("failed reading integration repo")?;
            let branch: String = row.get(3).context("failed reading integration branch")?;
            let installation_id_raw: Option<i64> =
                row.get(4).context("failed reading installation_id")?;

            items.push(GithubIntegrationSummary {
                app_id: Uuid::parse_str(&app_id_raw).with_context(|| {
                    format!("invalid app id in github integration: {app_id_raw}")
                })?,
                owner,
                repo,
                branch,
                installation_id: installation_id_raw.map(to_u64).transpose()?,
            });
        }

        Ok(items)
    }

    fn create_deployment_and_enqueue_job(
        &self,
        app_id: Uuid,
        request: &CreateDeploymentRequest,
        now_unix_ms: u64,
        max_attempts: u32,
    ) -> Result<CreateDeploymentAccepted> {
        let deployment_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let simulate_failures = request.simulate_failures.unwrap_or(0);

        let payload = DeployJobPayload {
            deployment_id,
            app_id,
            simulate_failures,
            source_ref: request.source_ref.clone(),
            commit_sha: request.commit_sha.clone(),
            force_rebuild: request.force_rebuild.unwrap_or(false),
        };
        let payload_json =
            serde_json::to_string(&payload).context("failed serializing job payload")?;

        let now = to_i64(now_unix_ms)?;

        let conn = self.conn.lock().expect("database mutex poisoned");

        conn.execute(
            "INSERT INTO deployments (
                id,
                app_id,
                source_ref,
                image_ref,
                commit_sha,
                status,
                simulate_failures,
                created_at_unix_ms,
                updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                deployment_id.to_string(),
                app_id.to_string(),
                request.source_ref.as_deref(),
                request.image_ref.as_deref(),
                request.commit_sha.as_deref(),
                DeploymentStatus::Queued.as_str(),
                to_i64(simulate_failures as u64)?,
                now
            ],
        )
        .context("failed inserting deployment")?;

        conn.execute(
            "INSERT INTO jobs (
                id,
                job_key,
                job_type,
                payload_json,
                status,
                attempt_count,
                max_attempts,
                next_run_unix_ms,
                created_at_unix_ms,
                updated_at_unix_ms
             ) VALUES (?1, ?2, 'deploy_app', ?3, 'queued', 0, ?4, ?5, ?5, ?5)",
            params![
                job_id.to_string(),
                format!("deploy:{}", deployment_id),
                payload_json,
                to_i64(max_attempts as u64)?,
                now
            ],
        )
        .context("failed inserting deployment job")?;

        Ok(CreateDeploymentAccepted {
            deployment_id,
            status: DeploymentStatus::Queued,
            queued_at_unix_ms: now_unix_ms,
        })
    }

    fn list_deployments(&self, app_id: Uuid) -> Result<Vec<DeploymentSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, app_id, source_ref, image_ref, commit_sha, status, last_error, created_at_unix_ms, updated_at_unix_ms
                 FROM deployments
                 WHERE app_id = ?1
                 ORDER BY created_at_unix_ms DESC",
            )
            .context("failed preparing deployment listing query")?;

        let mut rows = stmt
            .query(params![app_id.to_string()])
            .context("failed querying deployments")?;
        let mut items = Vec::new();

        while let Some(row) = rows.next().context("failed iterating deployments")? {
            let deployment_id_raw: String = row.get(0).context("failed reading deployment id")?;
            let app_id_raw: String = row.get(1).context("failed reading deployment app id")?;
            let source_ref: Option<String> = row.get(2).context("failed reading source ref")?;
            let image_ref: Option<String> = row.get(3).context("failed reading image ref")?;
            let commit_sha: Option<String> = row.get(4).context("failed reading commit sha")?;
            let status_raw: String = row.get(5).context("failed reading deployment status")?;
            let last_error: Option<String> = row.get(6).context("failed reading last error")?;
            let created_raw: i64 = row.get(7).context("failed reading created time")?;
            let updated_raw: i64 = row.get(8).context("failed reading updated time")?;

            let id = Uuid::parse_str(&deployment_id_raw)
                .with_context(|| format!("invalid deployment id in db: {deployment_id_raw}"))?;
            let parsed_app_id = Uuid::parse_str(&app_id_raw)
                .with_context(|| format!("invalid app id in deployment row: {app_id_raw}"))?;
            let status = DeploymentStatus::parse(&status_raw)
                .with_context(|| format!("invalid deployment status in db: {status_raw}"))?;

            items.push(DeploymentSummary {
                id,
                app_id: parsed_app_id,
                source_ref,
                image_ref,
                commit_sha,
                status,
                last_error,
                created_at_unix_ms: to_u64(created_raw)?,
                updated_at_unix_ms: to_u64(updated_raw)?,
            });
        }

        Ok(items)
    }

    fn latest_deployment(&self, app_id: Uuid) -> Result<Option<DeploymentSummary>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT id, app_id, source_ref, image_ref, commit_sha, status, last_error, created_at_unix_ms, updated_at_unix_ms
                 FROM deployments
                 WHERE app_id = ?1
                 ORDER BY created_at_unix_ms DESC
                 LIMIT 1",
                params![app_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()
            .context("failed querying latest deployment")?;

        let Some((
            deployment_id_raw,
            app_id_raw,
            source_ref,
            image_ref,
            commit_sha,
            status_raw,
            last_error,
            created_raw,
            updated_raw,
        )) = row
        else {
            return Ok(None);
        };

        let status = DeploymentStatus::parse(&status_raw)
            .with_context(|| format!("invalid deployment status in db: {status_raw}"))?;

        Ok(Some(DeploymentSummary {
            id: Uuid::parse_str(&deployment_id_raw)
                .with_context(|| format!("invalid deployment id in db: {deployment_id_raw}"))?,
            app_id: Uuid::parse_str(&app_id_raw)
                .with_context(|| format!("invalid app id in db: {app_id_raw}"))?,
            source_ref,
            image_ref,
            commit_sha,
            status,
            last_error,
            created_at_unix_ms: to_u64(created_raw)?,
            updated_at_unix_ms: to_u64(updated_raw)?,
        }))
    }

    fn latest_deployment_id_for_app(&self, app_id: Uuid) -> Result<Option<Uuid>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let row = conn
            .query_row(
                "SELECT id FROM deployments WHERE app_id = ?1 ORDER BY created_at_unix_ms DESC LIMIT 1",
                params![app_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("failed querying latest deployment for app")?;
        match row {
            None => Ok(None),
            Some(id_raw) => {
                Ok(Some(Uuid::parse_str(&id_raw).with_context(|| {
                    format!("invalid deployment id in db: {id_raw}")
                })?))
            }
        }
    }

    fn run_reconciler_once(
        &self,
        now_unix_ms: u64,
        backoff_base_ms: u64,
        backoff_max_ms: u64,
    ) -> Result<bool> {
        let Some(job) = self.claim_due_job(now_unix_ms)? else {
            return Ok(false);
        };

        match job.job_type.as_str() {
            "deploy_app" => {
                if let Err(error) = self.execute_deploy_job(&job, now_unix_ms) {
                    let error_message = error.to_string();
                    self.retry_or_fail_deploy_job(
                        &job,
                        now_unix_ms,
                        &error_message,
                        backoff_base_ms,
                        backoff_max_ms,
                    )?;
                } else {
                    self.mark_job_success(job.id, now_unix_ms)?;
                }
            }
            unknown => {
                self.mark_job_failed(
                    job.id,
                    now_unix_ms,
                    &format!("unsupported job type: {unknown}"),
                )?;
            }
        }

        Ok(true)
    }

    fn execute_deploy_job(&self, job: &JobRecord, now_unix_ms: u64) -> Result<()> {
        let payload: DeployJobPayload =
            serde_json::from_str(&job.payload_json).context("failed parsing deploy job payload")?;

        if self
            .deployment_status(payload.deployment_id)?
            .is_some_and(|status| status == DeploymentStatus::Healthy)
        {
            return Ok(());
        }

        self.set_deployment_state(
            payload.deployment_id,
            DeploymentStatus::Deploying,
            None,
            now_unix_ms,
        )?;

        self.append_deployment_log(payload.deployment_id, "deployment started", now_unix_ms)?;
        if job.attempt_count <= payload.simulate_failures {
            self.append_deployment_log(
                payload.deployment_id,
                &format!(
                    "simulated failure on attempt {} of {}",
                    job.attempt_count, payload.simulate_failures
                ),
                now_unix_ms,
            )?;
            anyhow::bail!(
                "simulated deployment failure on attempt {} of {}",
                job.attempt_count,
                payload.simulate_failures
            );
        }

        if let Some(config) = self.get_import_config(payload.app_id)? {
            self.append_deployment_log(
                payload.deployment_id,
                &format!("build mode: {}", config.build_mode),
                now_unix_ms,
            )?;
            self.append_deployment_log(
                payload.deployment_id,
                &format!(
                    "detected framework={} package_manager={}",
                    config.detection.framework, config.detection.package_manager
                ),
                now_unix_ms,
            )?;
            if let Some(compose) = config.compose.as_ref() {
                let app_service = compose.app_service.as_deref().unwrap_or("unknown");
                self.append_deployment_log(
                    payload.deployment_id,
                    &format!(
                        "compose file={} app_service={} services={}",
                        compose.file,
                        app_service,
                        compose.services.len()
                    ),
                    now_unix_ms,
                )?;
            }
            if let Some(profile) = config.dependency_profile.as_ref() {
                if profile.postgres == Some(true) {
                    if let Some(image) =
                        compose_image_for_dependency(config.compose.as_ref(), "postgres")
                    {
                        self.append_deployment_log(
                            payload.deployment_id,
                            &format!("compose dependency for postgres uses image {image}"),
                            now_unix_ms,
                        )?;
                    } else {
                        let postgres =
                            self.ensure_managed_service(payload.app_id, "postgres", now_unix_ms)?;
                        if !postgres.healthy {
                            anyhow::bail!("postgres managed service is unhealthy");
                        }
                        let _database_url = format!(
                            "postgres://{}:{}@{}:{}/app",
                            postgres.username, postgres.password, postgres.host, postgres.port
                        );
                        self.append_deployment_log(
                            payload.deployment_id,
                            &format!(
                                "managed dependency ready: postgres at {}:{}",
                                postgres.host, postgres.port
                            ),
                            now_unix_ms,
                        )?;
                    }
                }
                if profile.redis == Some(true) {
                    if let Some(image) =
                        compose_image_for_dependency(config.compose.as_ref(), "redis")
                    {
                        self.append_deployment_log(
                            payload.deployment_id,
                            &format!("compose dependency for redis uses image {image}"),
                            now_unix_ms,
                        )?;
                    } else {
                        let redis =
                            self.ensure_managed_service(payload.app_id, "redis", now_unix_ms)?;
                        if !redis.healthy {
                            anyhow::bail!("redis managed service is unhealthy");
                        }
                        let _redis_url = format!(
                            "redis://{}:{}@{}:{}",
                            redis.username, redis.password, redis.host, redis.port
                        );
                        self.append_deployment_log(
                            payload.deployment_id,
                            &format!(
                                "managed dependency ready: redis at {}:{}",
                                redis.host, redis.port
                            ),
                            now_unix_ms,
                        )?;
                    }
                }
            }
            if config.build_mode == "dockerfile" && !config.detection.dockerfile_present {
                self.append_deployment_log(
                    payload.deployment_id,
                    "dockerfile mode selected but Dockerfile was not detected",
                    now_unix_ms,
                )?;
                anyhow::bail!("dockerfile mode requires Dockerfile in repository");
            }
            if config.build_mode == "compose" && config.compose.is_none() {
                self.append_deployment_log(
                    payload.deployment_id,
                    "compose mode selected but docker compose file was not detected",
                    now_unix_ms,
                )?;
                anyhow::bail!("compose mode requires docker-compose.yml/compose.yml in repository");
            }

            if config.build_mode == "compose" {
                let resolved_commit = payload
                    .commit_sha
                    .clone()
                    .or_else(|| {
                        payload.source_ref.as_deref().and_then(|value| {
                            value.strip_prefix("github:").map(ToString::to_string)
                        })
                    })
                    .filter(|value| !value.trim().is_empty());
                if payload.force_rebuild {
                    self.append_deployment_log(
                        payload.deployment_id,
                        "force rebuild requested (compose build --no-cache)",
                        now_unix_ms,
                    )?;
                }
                let route = self.deploy_compose_runtime(
                    payload.app_id,
                    payload.deployment_id,
                    &resolved_commit,
                    payload.force_rebuild,
                    &config,
                    now_unix_ms,
                )?;
                self.upsert_app_runtime(&route, now_unix_ms)?;
                self.append_deployment_log(
                    payload.deployment_id,
                    &format!(
                        "runtime upstream endpoint {}:{}",
                        route.upstream_host, route.upstream_port
                    ),
                    now_unix_ms,
                )?;
                if let Err(error) = sync_caddyfile_from_db(self) {
                    warn!(%error, "failed syncing caddyfile after deployment");
                    self.append_deployment_log(
                        payload.deployment_id,
                        &format!("warning: failed to update routing: {error}"),
                        now_unix_ms,
                    )?;
                }
            } else {
                self.append_deployment_log(
                    payload.deployment_id,
                    "build pipeline step simulation complete",
                    now_unix_ms,
                )?;
            }
        }

        self.set_deployment_state(
            payload.deployment_id,
            DeploymentStatus::Healthy,
            None,
            now_unix_ms,
        )?;
        self.append_deployment_log(payload.deployment_id, "deployment healthy", now_unix_ms)?;
        Ok(())
    }

    fn deploy_compose_runtime(
        &self,
        app_id: Uuid,
        deployment_id: Uuid,
        resolved_commit: &Option<String>,
        force_rebuild: bool,
        config: &EffectiveAppConfigResponse,
        now_unix_ms: u64,
    ) -> Result<AppRuntimeRoute> {
        let compose = config
            .compose
            .as_ref()
            .context("compose deployment requested without compose metadata")?;
        let app_service = compose
            .app_service
            .clone()
            .context("compose app service could not be detected")?;
        let app_port = detect_app_service_port(compose, &app_service)
            .context("compose app service does not expose any parseable port")?;
        let project_name = compose_project_name(app_id);
        let runtime_root = runtime_root_path();
        let checkout_path = runtime_checkout_path(&runtime_root, app_id);
        let compose_file_path = checkout_path.join(&compose.file);
        let runtime_compose_path = checkout_path.join("rustploy.compose.runtime.yml");
        let branch = config
            .source
            .branch
            .as_deref()
            .unwrap_or("main")
            .to_string();
        let clone_url = config.repository.clone_url.clone().unwrap_or_else(|| {
            format!(
                "https://github.com/{}/{}.git",
                config.repository.owner, config.repository.name
            )
        });
        let app_env_vars = self
            .list_app_env_vars(app_id)?
            .into_iter()
            .map(|record| (record.key, record.value))
            .collect::<HashMap<_, _>>();
        let mut log_redactions = app_env_vars.values().cloned().collect::<Vec<_>>();
        log_redactions.push(clone_url.clone());
        if let Some(credentials) = credentials_fragment_from_url(&clone_url) {
            log_redactions.push(credentials);
        }

        self.append_deployment_log(
            deployment_id,
            &format!("runtime checkout: {}", checkout_path.display()),
            now_unix_ms,
        )?;
        if !app_env_vars.is_empty() {
            self.append_deployment_log(
                deployment_id,
                &format!(
                    "injecting {} app environment variables into compose runtime",
                    app_env_vars.len()
                ),
                now_unix_ms,
            )?;
        }
        if checkout_path.exists() && compose_file_path.exists() {
            if let Err(error) = write_runtime_compose_file(
                &compose_file_path,
                &runtime_compose_path,
                &app_service,
                app_port,
                &app_env_vars,
            ) {
                self.append_deployment_log(
                    deployment_id,
                    &format!("warning: failed preparing compose runtime file for down: {error}"),
                    now_unix_ms,
                )?;
            }
            let down_file = if runtime_compose_path.exists() {
                runtime_compose_path.as_path()
            } else {
                compose_file_path.as_path()
            };
            if let Err(error) = run_compose_command_with_live_logs(
                &checkout_path,
                &project_name,
                &[down_file],
                &["down", "--remove-orphans"],
                self,
                deployment_id,
                &log_redactions,
            ) {
                self.append_deployment_log(
                    deployment_id,
                    &format!("warning: compose down before deploy failed: {error}"),
                    now_unix_ms,
                )?;
            }
        }
        if checkout_path.exists() {
            fs::remove_dir_all(&checkout_path).with_context(|| {
                format!(
                    "failed clearing previous checkout {}",
                    checkout_path.display()
                )
            })?;
        }
        fs::create_dir_all(&checkout_path)
            .with_context(|| format!("failed creating checkout dir {}", checkout_path.display()))?;

        clone_repository_for_deploy_with_live_logs(
            &clone_url,
            &branch,
            &checkout_path,
            self,
            deployment_id,
            &log_redactions,
        )?;
        self.append_deployment_log(
            deployment_id,
            &format!("cloned {}@{}", config.repository.name, branch),
            now_unix_ms,
        )?;
        if let Some(commit) = resolved_commit.as_deref() {
            checkout_commit_for_deploy_with_live_logs(
                &checkout_path,
                commit,
                self,
                deployment_id,
                &log_redactions,
            )?;
            self.append_deployment_log(
                deployment_id,
                &format!("checked out commit {commit}"),
                now_unix_ms,
            )?;
        }
        let created_env_files = ensure_compose_env_files(&checkout_path, &compose.file)?;
        if !created_env_files.is_empty() {
            let rendered = created_env_files
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.append_deployment_log(
                deployment_id,
                &format!("created missing env files for compose: {rendered}"),
                now_unix_ms,
            )?;
        }

        if !compose_file_path.exists() {
            anyhow::bail!(
                "compose file {} does not exist in checkout",
                compose_file_path.display()
            );
        }
        write_runtime_compose_file(
            &compose_file_path,
            &runtime_compose_path,
            &app_service,
            app_port,
            &app_env_vars,
        )?;

        if force_rebuild {
            run_compose_command_with_live_logs(
                &checkout_path,
                &project_name,
                &[runtime_compose_path.as_path()],
                &["build", "--no-cache"],
                self,
                deployment_id,
                &log_redactions,
            )?;
            run_compose_command_with_live_logs(
                &checkout_path,
                &project_name,
                &[runtime_compose_path.as_path()],
                &["up", "-d", "--force-recreate", "--remove-orphans"],
                self,
                deployment_id,
                &log_redactions,
            )?;
        } else {
            run_compose_command_with_live_logs(
                &checkout_path,
                &project_name,
                &[runtime_compose_path.as_path()],
                &["up", "-d", "--build", "--remove-orphans"],
                self,
                deployment_id,
                &log_redactions,
            )?;
        }
        let mapped_port = resolve_compose_service_port(
            &checkout_path,
            &project_name,
            &[runtime_compose_path.as_path()],
            &app_service,
            app_port,
        )?;
        wait_for_compose_service_ready(
            &checkout_path,
            &project_name,
            &[runtime_compose_path.as_path()],
            &app_service,
            deployment_id,
            now_unix_ms,
            self,
        )?;

        Ok(AppRuntimeRoute {
            app_id,
            deployment_id,
            project_name,
            service_name: app_service,
            upstream_host: runtime_upstream_host(),
            upstream_port: mapped_port,
        })
    }

    fn retry_or_fail_deploy_job(
        &self,
        job: &JobRecord,
        now_unix_ms: u64,
        error_message: &str,
        backoff_base_ms: u64,
        backoff_max_ms: u64,
    ) -> Result<()> {
        let payload: DeployJobPayload =
            serde_json::from_str(&job.payload_json).context("failed parsing deploy job payload")?;

        if job.attempt_count < job.max_attempts {
            let delay_ms = compute_backoff_ms(job.attempt_count, backoff_base_ms, backoff_max_ms);
            let next_run = now_unix_ms.saturating_add(delay_ms);

            self.mark_job_retry(job.id, next_run, now_unix_ms, error_message)?;
            self.append_deployment_log(payload.deployment_id, error_message, now_unix_ms)?;
            self.set_deployment_state(
                payload.deployment_id,
                DeploymentStatus::Retrying,
                Some(error_message),
                now_unix_ms,
            )?;
        } else {
            self.mark_job_failed(job.id, now_unix_ms, error_message)?;
            self.append_deployment_log(payload.deployment_id, error_message, now_unix_ms)?;
            self.set_deployment_state(
                payload.deployment_id,
                DeploymentStatus::Failed,
                Some(error_message),
                now_unix_ms,
            )?;
        }

        Ok(())
    }

    fn claim_due_job(&self, now_unix_ms: u64) -> Result<Option<JobRecord>> {
        let now = to_i64(now_unix_ms)?;
        let mut conn = self.conn.lock().expect("database mutex poisoned");
        let tx = conn.transaction().context("failed to open transaction")?;

        let maybe_job = tx
            .query_row(
                "SELECT id, job_type, payload_json, attempt_count, max_attempts
                 FROM jobs
                 WHERE status = 'queued' AND next_run_unix_ms <= ?1
                 ORDER BY created_at_unix_ms ASC
                 LIMIT 1",
                params![now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .context("failed querying next queued job")?;

        let Some((id_raw, job_type, payload_json, attempt_count_raw, max_attempts_raw)) = maybe_job
        else {
            tx.commit().context("failed committing empty transaction")?;
            return Ok(None);
        };

        let job_id =
            Uuid::parse_str(&id_raw).with_context(|| format!("invalid job id in db: {id_raw}"))?;
        let updated = tx
            .execute(
                "UPDATE jobs
                 SET status = 'running',
                     attempt_count = attempt_count + 1,
                     locked_at_unix_ms = ?2,
                     updated_at_unix_ms = ?2
                 WHERE id = ?1 AND status = 'queued'",
                params![job_id.to_string(), now],
            )
            .context("failed claiming queued job")?;

        tx.commit().context("failed committing claim transaction")?;

        if updated == 0 {
            return Ok(None);
        }

        Ok(Some(JobRecord {
            id: job_id,
            job_type,
            payload_json,
            attempt_count: to_u32(attempt_count_raw + 1)?,
            max_attempts: to_u32(max_attempts_raw)?,
        }))
    }

    fn mark_job_retry(
        &self,
        job_id: Uuid,
        next_run_unix_ms: u64,
        now_unix_ms: u64,
        error_message: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE jobs
             SET status = 'queued',
                 next_run_unix_ms = ?2,
                 last_error = ?3,
                 locked_at_unix_ms = NULL,
                 updated_at_unix_ms = ?4
             WHERE id = ?1",
            params![
                job_id.to_string(),
                to_i64(next_run_unix_ms)?,
                error_message,
                to_i64(now_unix_ms)?
            ],
        )
        .context("failed marking job for retry")?;

        Ok(())
    }

    fn mark_job_success(&self, job_id: Uuid, now_unix_ms: u64) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE jobs
             SET status = 'succeeded',
                 last_error = NULL,
                 locked_at_unix_ms = NULL,
                 completed_at_unix_ms = ?2,
                 updated_at_unix_ms = ?2
             WHERE id = ?1",
            params![job_id.to_string(), to_i64(now_unix_ms)?],
        )
        .context("failed marking job as succeeded")?;

        Ok(())
    }

    fn mark_job_failed(&self, job_id: Uuid, now_unix_ms: u64, error_message: &str) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE jobs
             SET status = 'failed',
                 last_error = ?2,
                 locked_at_unix_ms = NULL,
                 completed_at_unix_ms = ?3,
                 updated_at_unix_ms = ?3
             WHERE id = ?1",
            params![job_id.to_string(), error_message, to_i64(now_unix_ms)?],
        )
        .context("failed marking job as failed")?;

        Ok(())
    }

    fn set_deployment_state(
        &self,
        deployment_id: Uuid,
        status: DeploymentStatus,
        last_error: Option<&str>,
        now_unix_ms: u64,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE deployments
             SET status = ?2,
                 last_error = ?3,
                 updated_at_unix_ms = ?4
             WHERE id = ?1",
            params![
                deployment_id.to_string(),
                status.as_str(),
                last_error,
                to_i64(now_unix_ms)?
            ],
        )
        .context("failed updating deployment state")?;

        Ok(())
    }

    fn deployment_status(&self, deployment_id: Uuid) -> Result<Option<DeploymentStatus>> {
        let conn = self.conn.lock().expect("database mutex poisoned");

        let status_raw: Option<String> = conn
            .query_row(
                "SELECT status FROM deployments WHERE id = ?1",
                params![deployment_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .context("failed querying deployment status")?;

        match status_raw {
            None => Ok(None),
            Some(value) => DeploymentStatus::parse(&value)
                .map(Some)
                .with_context(|| format!("invalid deployment status in db: {value}")),
        }
    }

    fn append_deployment_log(
        &self,
        deployment_id: Uuid,
        message: &str,
        now_unix_ms: u64,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO deployment_logs (deployment_id, message, created_at_unix_ms)
             VALUES (?1, ?2, ?3)",
            params![deployment_id.to_string(), message, to_i64(now_unix_ms)?],
        )
        .context("failed writing deployment log")?;
        Ok(())
    }

    fn deployment_log_output(&self, deployment_id: Uuid) -> Result<String> {
        let rows = self.deployment_log_rows_since(deployment_id, None)?;
        Ok(rows
            .into_iter()
            .map(|row| row.message)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn deployment_log_rows_since(
        &self,
        deployment_id: Uuid,
        last_log_id: Option<i64>,
    ) -> Result<Vec<DeploymentLogRow>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut entries = Vec::new();

        if let Some(after_id) = last_log_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id, message
                     FROM deployment_logs
                     WHERE deployment_id = ?1
                       AND id > ?2
                     ORDER BY id ASC",
                )
                .context("failed preparing incremental deployment log query")?;
            let mut rows = stmt
                .query(params![deployment_id.to_string(), after_id])
                .context("failed querying incremental deployment logs")?;
            while let Some(row) = rows
                .next()
                .context("failed iterating incremental deployment logs")?
            {
                entries.push(DeploymentLogRow {
                    id: row.get(0).context("failed reading deployment log id")?,
                    message: row
                        .get(1)
                        .context("failed reading deployment log message")?,
                });
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, message
                     FROM deployment_logs
                     WHERE deployment_id = ?1
                     ORDER BY id ASC",
                )
                .context("failed preparing deployment log query")?;
            let mut rows = stmt
                .query(params![deployment_id.to_string()])
                .context("failed querying deployment logs")?;
            while let Some(row) = rows.next().context("failed iterating deployment logs")? {
                entries.push(DeploymentLogRow {
                    id: row.get(0).context("failed reading deployment log id")?,
                    message: row
                        .get(1)
                        .context("failed reading deployment log message")?,
                });
            }
        }

        Ok(entries)
    }

    #[cfg(test)]
    fn force_jobs_due_for_tests(&self) {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let _ = conn.execute(
            "UPDATE jobs SET next_run_unix_ms = 0 WHERE status = 'queued'",
            [],
        );
    }
}

fn initialize_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("failed setting journal_mode=WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("failed enabling foreign keys")?;
    conn.execute_batch("PRAGMA busy_timeout = 5000;")
        .context("failed setting sqlite busy_timeout")?;
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))
        .context("failed running sqlite migration 0001")?;
    conn.execute_batch(include_str!("../migrations/0002_jobs.sql"))
        .context("failed running sqlite migration 0002")?;
    conn.execute_batch(include_str!("../migrations/0003_github.sql"))
        .context("failed running sqlite migration 0003")?;
    conn.execute_batch(include_str!("../migrations/0004_api_tokens_and_import.sql"))
        .context("failed running sqlite migration 0004")?;
    conn.execute_batch(include_str!("../migrations/0005_deployment_logs.sql"))
        .context("failed running sqlite migration 0005")?;
    conn.execute_batch(include_str!("../migrations/0006_auth_domains_services.sql"))
        .context("failed running sqlite migration 0006")?;
    conn.execute_batch(include_str!("../migrations/0007_compose_support.sql"))
        .context("failed running sqlite migration 0007")?;
    conn.execute_batch(include_str!("../migrations/0008_runtime_routing.sql"))
        .context("failed running sqlite migration 0008")?;
    conn.execute_batch(include_str!("../migrations/0009_app_env_vars.sql"))
        .context("failed running sqlite migration 0009")?;
    conn.execute_batch(include_str!(
        "../migrations/0010_deployment_logs_cursor_index.sql"
    ))
    .context("failed running sqlite migration 0010")?;
    ensure_deployments_metadata_columns(conn)?;
    ensure_import_config_columns(conn)?;

    Ok(())
}

fn ensure_deployments_metadata_columns(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "deployments", "image_ref")? {
        conn.execute_batch("ALTER TABLE deployments ADD COLUMN image_ref TEXT;")
            .context("failed adding deployments.image_ref column")?;
    }
    if !column_exists(conn, "deployments", "commit_sha")? {
        conn.execute_batch("ALTER TABLE deployments ADD COLUMN commit_sha TEXT;")
            .context("failed adding deployments.commit_sha column")?;
    }
    Ok(())
}

fn ensure_import_config_columns(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "app_import_configs", "compose_json")? {
        conn.execute_batch("ALTER TABLE app_import_configs ADD COLUMN compose_json TEXT;")
            .context("failed adding app_import_configs.compose_json column")?;
    }
    if !column_exists(conn, "app_import_configs", "repo_clone_url")? {
        conn.execute_batch("ALTER TABLE app_import_configs ADD COLUMN repo_clone_url TEXT;")
            .context("failed adding app_import_configs.repo_clone_url column")?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let query = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&query)
        .context("failed preparing table_info query")?;
    let mut rows = stmt.query([]).context("failed querying table_info")?;
    while let Some(row) = rows.next().context("failed iterating table_info")? {
        let name: String = row
            .get(1)
            .context("failed reading table_info column name")?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_env_u64(name: &str, default: u64) -> Result<u64> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .parse::<u64>()
                .with_context(|| format!("invalid {name}"))
        })
        .unwrap_or_else(|| Ok(default))
}

fn read_env_u32(name: &str, default: u32) -> Result<u32> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .parse::<u32>()
                .with_context(|| format!("invalid {name}"))
        })
        .unwrap_or_else(|| Ok(default))
}

fn read_env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("value does not fit in i64")
}

fn to_u64(value: i64) -> Result<u64> {
    u64::try_from(value).context("value does not fit in u64")
}

fn to_u32(value: i64) -> Result<u32> {
    u32::try_from(value).context("value does not fit in u32")
}

fn to_u16(value: i64) -> Result<u16> {
    u16::try_from(value).context("value does not fit in u16")
}

fn compute_backoff_ms(attempt: u32, base_ms: u64, max_ms: u64) -> u64 {
    let shift = attempt.saturating_sub(1).min(16);
    base_ms.saturating_mul(1u64 << shift).min(max_ms)
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_prometheus))
        .route("/api/v1/health", get(health))
        .route("/", get(dashboard_ui))
        .route("/logs", get(logs_ui))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route(
            "/api/v1/auth/password-reset/request",
            post(auth_password_reset_request),
        )
        .route(
            "/api/v1/auth/password-reset/confirm",
            post(auth_password_reset_confirm),
        )
        .route("/api/v1/apps/import", post(import_app))
        .route("/api/v1/apps", get(list_apps).post(create_app))
        .route("/api/v1/apps/:app_id/github", post(connect_github_repo))
        .route("/api/v1/apps/:app_id/config", get(get_app_effective_config))
        .route(
            "/api/v1/apps/:app_id/env",
            get(list_app_env_vars).put(upsert_app_env_var),
        )
        .route("/api/v1/apps/:app_id/env/:key", delete(delete_app_env_var))
        .route(
            "/api/v1/apps/:app_id/domains",
            get(list_domains).post(create_domain),
        )
        .route("/api/v1/apps/:app_id/logs/stream", get(stream_app_logs))
        .route(
            "/api/v1/apps/:app_id/deployments",
            get(list_app_deployments).post(create_deployment),
        )
        .route(
            "/api/v1/apps/:app_id/deployments/:deployment_id/logs",
            get(get_deployment_logs),
        )
        .route("/api/v1/apps/:app_id/rollback", post(rollback_deployment))
        .route(
            "/api/v1/integrations/github/webhook",
            post(handle_github_webhook),
        )
        .route("/api/v1/tokens", get(list_tokens).post(create_token))
        .route("/api/v1/tokens/:token_id", delete(revoke_token))
        .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/agents/register", post(register_agent))
        .route("/api/v1/agents/heartbeat", post(agent_heartbeat))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::ok())
}

async fn metrics_prometheus(
    State(state): State<AppState>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let metrics = state.db.metrics_snapshot().map_err(internal_error)?;
    let body = format!(
        "# HELP rustploy_apps_total Total apps\n# TYPE rustploy_apps_total gauge\nrustploy_apps_total {}\n\
         # HELP rustploy_deployments_total Total deployments\n# TYPE rustploy_deployments_total gauge\nrustploy_deployments_total {}\n\
         # HELP rustploy_deployments_healthy Healthy deployments\n# TYPE rustploy_deployments_healthy gauge\nrustploy_deployments_healthy {}\n\
         # HELP rustploy_deployments_failed Failed deployments\n# TYPE rustploy_deployments_failed gauge\nrustploy_deployments_failed {}\n\
         # HELP rustploy_deployments_queued Queued/deploying/retrying deployments\n# TYPE rustploy_deployments_queued gauge\nrustploy_deployments_queued {}\n\
         # HELP rustploy_domains_total Total mapped domains\n# TYPE rustploy_domains_total gauge\nrustploy_domains_total {}\n\
         # HELP rustploy_tokens_total Active API tokens\n# TYPE rustploy_tokens_total gauge\nrustploy_tokens_total {}\n",
        metrics.apps_total,
        metrics.deployments_total,
        metrics.deployments_healthy,
        metrics.deployments_failed,
        metrics.deployments_queued,
        metrics.domains_total,
        metrics.tokens_total
    );
    Ok((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    ))
}

async fn dashboard_ui(State(state): State<AppState>, headers: HeaderMap) -> Html<String> {
    if let Some(host) = request_host(&headers) {
        match resolve_app_for_host(&state, &host) {
            Ok(Some(app)) => {
                let latest_deployment = state.db.latest_deployment(app.id).ok().flatten();
                let config = state.db.get_import_config(app.id).ok().flatten();
                return Html(render_app_frontdoor(
                    &host,
                    &app,
                    latest_deployment.as_ref(),
                    config.as_ref(),
                ));
            }
            Ok(None) => {}
            Err(error) => {
                warn!(%error, host, "failed resolving host app route");
            }
        }
    }

    let session = authorize_session_request(&state, &headers).ok().flatten();
    if session.is_some() {
        Html(DASHBOARD_HTML.to_string())
    } else {
        Html(LOGIN_HTML.to_string())
    }
}

async fn logs_ui(State(state): State<AppState>, headers: HeaderMap) -> Html<String> {
    let session = authorize_session_request(&state, &headers).ok().flatten();
    if session.is_some() {
        Html(LOGS_HTML.to_string())
    } else {
        Html(LOGIN_HTML.to_string())
    }
}

fn request_host(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(HOST_HEADER)?.to_str().ok()?;
    let first = value.split(',').next()?.trim();
    if first.is_empty() {
        return None;
    }

    let stripped = if first.starts_with('[') {
        first
            .strip_prefix('[')
            .and_then(|value| value.split(']').next())
            .unwrap_or(first)
    } else if let Some((host, port)) = first.rsplit_once(':') {
        if port.chars().all(|ch| ch.is_ascii_digit()) {
            host
        } else {
            first
        }
    } else {
        first
    };

    Some(stripped.trim().to_ascii_lowercase())
}

fn is_control_plane_host(host: &str) -> bool {
    host == CONTROL_PLANE_HOST
        || host == CONTROL_PLANE_ALT_HOST
        || host == "127.0.0.1"
        || host == "::1"
}

fn normalize_domain(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn is_valid_env_var_key(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_reserved_control_plane_domain(domain: &str) -> bool {
    let normalized = normalize_domain(domain);
    normalized == CONTROL_PLANE_HOST
        || normalized == CONTROL_PLANE_ALT_HOST
        || normalized == "127.0.0.1"
        || normalized == "::1"
}

fn resolve_app_for_host(state: &AppState, host: &str) -> Result<Option<AppSummary>> {
    if is_control_plane_host(host) {
        return Ok(None);
    }

    if let Some(mapped) = state.db.find_app_by_domain(host)? {
        return Ok(Some(mapped));
    }

    if let Some(app_name) = host.strip_suffix(".localhost") {
        if !app_name.is_empty() && !app_name.contains('.') {
            if let Some(app) = state.db.find_app_by_name(app_name)? {
                return Ok(Some(app));
            }
            for app in state.db.list_apps()? {
                if localhost_slug(&app.name) == app_name {
                    return Ok(Some(app));
                }
            }
        }
    }

    Ok(None)
}

fn localhost_slug(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else {
            None
        };
        match mapped {
            Some(valid) => {
                out.push(valid);
                last_dash = false;
            }
            None => {
                if !last_dash {
                    out.push('-');
                    last_dash = true;
                }
            }
        }
    }
    out.trim_matches('-').to_string()
}

fn render_app_frontdoor(
    host: &str,
    app: &AppSummary,
    deployment: Option<&DeploymentSummary>,
    config: Option<&EffectiveAppConfigResponse>,
) -> String {
    let app_name = escape_html(&app.name);
    let host = escape_html(host);
    let app_id = app.id;

    let (status, source_ref, badge_class) = if let Some(deployment) = deployment {
        (
            deployment.status.as_str().to_string(),
            deployment
                .source_ref
                .as_deref()
                .unwrap_or("manual")
                .to_string(),
            if deployment.status == DeploymentStatus::Healthy {
                "ok"
            } else if deployment.status == DeploymentStatus::Failed {
                "err"
            } else {
                "warn"
            },
        )
    } else {
        ("none".to_string(), "n/a".to_string(), "warn")
    };

    let profile = config
        .map(|value| {
            format!(
                "{} / {}",
                value.detection.framework, value.detection.package_manager
            )
        })
        .unwrap_or_else(|| "not imported".to_string());

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{app_name}</title>
  <style>
    @import url('https://fonts.googleapis.com/css2?family=Chakra+Petch:wght@400;500;700&family=JetBrains+Mono:wght@500&display=swap');
    :root {{
      --bg: #060b14;
      --grid: rgba(0, 245, 212, 0.06);
      --panel: #111c33;
      --line: #1f3655;
      --ink: #dffaff;
      --muted: #81a7ba;
      --ok: #7affea;
      --warn: #ffbe5c;
      --err: #ff6b7d;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      min-height: 100vh;
      color: var(--ink);
      font-family: 'Chakra Petch', sans-serif;
      background-color: var(--bg);
      background-image:
        linear-gradient(var(--grid) 1px, transparent 1px),
        linear-gradient(90deg, var(--grid) 1px, transparent 1px);
      background-size: 44px 44px;
      display: grid;
      place-items: center;
      padding: 18px;
    }}
    .panel {{
      width: min(860px, 100%);
      border: 1px solid var(--line);
      border-radius: 14px;
      background: rgba(13, 22, 40, 0.92);
      padding: 18px;
      box-shadow: 0 18px 50px rgba(0, 0, 0, 0.45);
    }}
    h1 {{ margin: 0 0 8px; font-size: 1.9rem; }}
    p {{ margin: 0 0 12px; color: var(--muted); }}
    .grid {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
      gap: 10px;
      margin: 12px 0 14px;
    }}
    .cell {{
      border: 1px solid var(--line);
      border-radius: 10px;
      padding: 10px;
      background: #0b1323;
      display: grid;
      gap: 4px;
    }}
    .cell strong {{ font-size: 0.78rem; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); }}
    .cell code {{ font-family: 'JetBrains Mono', monospace; font-size: 12px; }}
    .badge {{
      display: inline-flex;
      width: fit-content;
      border-radius: 999px;
      padding: 5px 9px;
      font-size: 11px;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: .05em;
      border: 1px solid #285763;
      color: var(--ok);
      background: rgba(0, 245, 212, 0.11);
    }}
    .badge.warn {{ color: var(--warn); border-color: #78552a; background: rgba(255, 190, 92, 0.12); }}
    .badge.err {{ color: var(--err); border-color: #8d3346; background: rgba(255, 107, 125, 0.12); }}
    .tip {{
      margin-top: 12px;
      font-size: 0.9rem;
      color: var(--muted);
    }}
    a {{
      color: #7affea;
      text-decoration: none;
      border-bottom: 1px dashed rgba(122, 255, 234, 0.4);
    }}
  </style>
</head>
<body>
  <main class="panel">
    <h1>{app_name}</h1>
    <p>App edge endpoint for <code>{host}</code>.</p>
    <span class="badge {badge_class}">{status}</span>
    <div class="grid">
      <div class="cell"><strong>App ID</strong><code>{app_id}</code></div>
      <div class="cell"><strong>Source</strong><code>{source_ref}</code></div>
      <div class="cell"><strong>Profile</strong><code>{profile}</code></div>
      <div class="cell"><strong>Control Plane</strong><code><a href="https://localhost/">https://localhost</a></code></div>
    </div>
    <p class="tip">This frontdoor confirms host routing. Latest healthy compose deployments are served directly by runtime containers.</p>
  </main>
</body>
</html>"#,
        app_name = app_name,
        host = host,
        badge_class = badge_class,
        status = escape_html(&status),
        app_id = app_id,
        source_ref = escape_html(&source_ref),
        profile = escape_html(&profile),
    )
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

type ApiJsonError = (StatusCode, Json<ApiErrorResponse>);

const LOGIN_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Rustploy Login</title>
  <style>
    @import url('https://fonts.googleapis.com/css2?family=Chakra+Petch:wght@400;500;700&family=JetBrains+Mono:wght@500&display=swap');
    :root {
      --bg: #060b14;
      --bg-grid: rgba(0, 245, 212, 0.07);
      --panel: #0d1628;
      --panel-2: #101c33;
      --ink: #dcf9ff;
      --muted: #83a5b7;
      --line: #1f3655;
      --neon: #00f5d4;
      --neon-ink: #032a27;
      --danger: #ff6b7d;
      --soft-shadow: 0 22px 60px rgba(0, 0, 0, 0.45);
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      color: var(--ink);
      font-family: 'Chakra Petch', sans-serif;
      background-color: var(--bg);
      background-image:
        linear-gradient(var(--bg-grid) 1px, transparent 1px),
        linear-gradient(90deg, var(--bg-grid) 1px, transparent 1px);
      background-size: 44px 44px;
      display: grid;
      place-items: center;
      padding: 28px 16px;
    }
    .layout {
      width: min(980px, 100%);
      border: 1px solid var(--line);
      border-radius: 18px;
      background: rgba(8, 13, 23, 0.88);
      box-shadow: var(--soft-shadow);
      overflow: hidden;
      display: grid;
      grid-template-columns: 1.2fr 1fr;
      animation: panel-in 220ms ease-out;
    }
    .hero {
      padding: 34px;
      border-right: 1px solid var(--line);
      background: var(--panel);
    }
    .badge {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      padding: 6px 10px;
      border-radius: 999px;
      border: 1px solid #1f8e82;
      background: rgba(0, 245, 212, 0.12);
      color: #8cfcee;
      font-size: 12px;
      letter-spacing: 0.06em;
      text-transform: uppercase;
      font-weight: 700;
    }
    .hero h1 {
      margin: 16px 0 10px;
      font-size: clamp(1.9rem, 3vw, 2.7rem);
      line-height: 1.05;
      text-wrap: balance;
    }
    .hero p {
      margin: 0;
      color: var(--muted);
      line-height: 1.5;
      max-width: 42ch;
    }
    .mono {
      margin-top: 28px;
      font-family: 'JetBrains Mono', monospace;
      font-size: 12px;
      color: #9fc6d6;
      border: 1px solid #1f3655;
      border-radius: 10px;
      padding: 10px 12px;
      background: rgba(16, 28, 51, 0.7);
    }
    .card {
      padding: 30px 24px;
      background: var(--panel-2);
    }
    .card h2 {
      margin: 0 0 8px;
      font-size: 1.3rem;
      letter-spacing: 0.01em;
    }
    .card p {
      margin: 0 0 14px;
      color: var(--muted);
      font-size: 0.95rem;
    }
    label {
      display: block;
      font-size: 13px;
      font-weight: 600;
      margin: 12px 0 6px;
      color: #c3e7f7;
    }
    input, button {
      width: 100%;
      border-radius: 10px;
      padding: 11px 12px;
      font: inherit;
    }
    input {
      border: 1px solid var(--line);
      background: #0a1221;
      color: var(--ink);
    }
    input::placeholder { color: #6389a0; }
    input:focus {
      outline: none;
      border-color: #36d7c2;
      box-shadow: 0 0 0 3px rgba(0, 245, 212, 0.17);
    }
    button {
      margin-top: 14px;
      border: 0;
      font-weight: 700;
      color: var(--neon-ink);
      background: var(--neon);
      cursor: pointer;
      transition: transform .08s ease, filter .2s ease;
      box-shadow: 0 0 24px rgba(0, 245, 212, 0.3);
    }
    button:hover { filter: brightness(1.03); }
    button:active { transform: translateY(1px); }
    .err {
      min-height: 1.1em;
      margin-top: 10px;
      font-size: 13px;
      color: var(--danger);
    }
    @keyframes panel-in {
      from { opacity: 0; transform: translateY(8px); }
      to { opacity: 1; transform: translateY(0); }
    }
    @media (max-width: 860px) {
      .layout { grid-template-columns: 1fr; }
      .hero { border-right: 0; border-bottom: 1px solid var(--line); }
    }
  </style>
</head>
<body>
  <main class="layout">
    <section class="hero">
      <div class="badge">Rustploy Control Plane</div>
      <h1>Low-overhead deployment ops for self-hosted teams.</h1>
      <p>
        Sign in with your owner account to import repositories, trigger deployments,
        inspect logs, and manage domains from one panel.
      </p>
      <div class="mono">Tip: default local login is admin@localhost / admin</div>
    </section>
    <form class="card" id="login-form">
      <h2>Sign in</h2>
      <p>Authenticate with your local owner account.</p>
      <label for="email">Email</label>
      <input id="email" type="email" placeholder="admin@localhost" required />
      <label for="password">Password</label>
      <input id="password" type="password" placeholder="password" required />
      <button id="submit" type="submit">Enter Dashboard</button>
      <div class="err" id="err"></div>
    </form>
  </main>
  <script>
    document.getElementById('login-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      const submit = document.getElementById('submit');
      const err = document.getElementById('err');
      err.textContent = '';
      submit.disabled = true;
      submit.textContent = 'Signing in...';
      const email = document.getElementById('email').value;
      const password = document.getElementById('password').value;
      const res = await fetch('/api/v1/auth/login', {
        method: 'POST',
        credentials: 'include',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ email, password })
      });
      if (res.ok) {
        window.location.href = '/';
      } else {
        err.textContent = 'Invalid credentials';
        submit.disabled = false;
        submit.textContent = 'Enter Dashboard';
      }
    });
  </script>
</body>
</html>
"#;

const DASHBOARD_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Rustploy Dashboard</title>
  <style>
    @import url('https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600;700&family=Geist+Mono:wght@500&display=swap');
    :root {
      --bg: oklch(0.13 0.005 260);
      --foreground: oklch(0.95 0 0);
      --card: oklch(0.16 0.005 260);
      --sidebar: oklch(0.11 0.005 260);
      --sidebar-accent: oklch(0.18 0.008 260);
      --secondary: oklch(0.2 0.005 260);
      --line: oklch(0.24 0.008 260);
      --muted: oklch(0.55 0.01 260);
      --primary: oklch(0.65 0.2 145);
      --danger: oklch(0.55 0.2 25);
      --warning: oklch(0.75 0.15 65);
      --chart-2: oklch(0.6 0.15 250);
      --chart-3: oklch(0.7 0.15 55);
      --ok: oklch(0.65 0.2 145);
      --shadow: 0 24px 60px rgba(0, 0, 0, 0.4);
    }
    * { box-sizing: border-box; }
    html {
      min-height: 100%;
      background: var(--bg);
      overscroll-behavior-y: none;
    }
    body {
      margin: 0;
      min-height: 100vh;
      color: var(--foreground);
      font-family: 'Geist', ui-sans-serif, system-ui, -apple-system, sans-serif;
      overscroll-behavior-y: none;
      background:
        radial-gradient(1200px 560px at 100% -140px, rgba(89, 97, 249, 0.12), transparent 70%),
        radial-gradient(950px 500px at -10% 0%, rgba(39, 191, 122, 0.14), transparent 60%),
        linear-gradient(var(--bg), var(--bg));
    }
    .shell {
      min-height: 100vh;
      display: grid;
      grid-template-columns: 244px minmax(0, 1fr);
    }
    .sidebar {
      background: color-mix(in oklab, var(--sidebar) 90%, black 10%);
      border-right: 1px solid var(--line);
      padding: 16px 12px;
      display: flex;
      flex-direction: column;
      gap: 12px;
      min-height: 100vh;
      position: sticky;
      top: 0;
    }
    .brand-block {
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 2px 6px 14px;
      border-bottom: 1px solid var(--line);
    }
    .brand-icon {
      width: 32px;
      height: 32px;
      border-radius: 9px;
      background: color-mix(in oklab, var(--primary) 82%, white 18%);
      color: #081810;
      display: grid;
      place-items: center;
      font-size: 16px;
      font-weight: 700;
    }
    .brand-title { margin: 0; font-size: 13px; font-weight: 600; }
    .brand-sub { margin: 1px 0 0; font-size: 11px; color: var(--muted); }
    .search {
      border: 1px solid var(--line);
      border-radius: 10px;
      background: color-mix(in oklab, var(--secondary) 92%, black 8%);
      color: var(--muted);
      font-size: 12px;
      padding: 8px 10px;
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 8px;
    }
    .search code {
      font-family: 'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
      border: 1px solid var(--line);
      border-radius: 6px;
      background: color-mix(in oklab, var(--secondary) 70%, black 30%);
      padding: 2px 6px;
      font-size: 10px;
    }
    .side-group {
      display: grid;
      gap: 4px;
      margin-top: 8px;
    }
    .side-label {
      padding: 0 8px 3px;
      font-size: 10px;
      font-weight: 600;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
    }
    .side-item {
      border: 1px solid transparent;
      border-radius: 9px;
      background: transparent;
      color: var(--muted);
      font-size: 12px;
      text-align: left;
      padding: 9px 10px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      box-shadow: none;
      font-weight: 500;
      cursor: pointer;
    }
    .side-item strong { color: inherit; }
    .side-item.active {
      background: color-mix(in oklab, var(--primary) 14%, transparent);
      border-color: color-mix(in oklab, var(--primary) 24%, var(--line));
      color: color-mix(in oklab, var(--primary) 80%, white 20%);
      font-weight: 600;
    }
    .side-count {
      font-size: 10px;
      border-radius: 999px;
      background: color-mix(in oklab, var(--secondary) 88%, black 12%);
      border: 1px solid var(--line);
      padding: 2px 7px;
      color: var(--muted);
      font-family: 'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
    }
    .sidebar-bottom {
      margin-top: auto;
      display: grid;
      gap: 10px;
      padding-top: 10px;
      border-top: 1px solid var(--line);
    }
    .new-project {
      width: 100%;
      border-radius: 10px;
      border: 0;
      box-shadow: none;
      padding: 10px 12px;
      background: color-mix(in oklab, var(--primary) 88%, white 12%);
      color: #07170f;
      font-weight: 600;
      font-size: 12px;
      text-align: center;
    }
    .profile {
      border: 1px solid var(--line);
      border-radius: 10px;
      padding: 8px;
      display: grid;
      gap: 2px;
      background: color-mix(in oklab, var(--secondary) 85%, black 15%);
    }
    .profile strong { font-size: 12px; }
    .profile span { font-size: 11px; color: var(--muted); }
    .main {
      padding: 18px;
      min-width: 0;
      display: grid;
      gap: 12px;
      align-content: start;
    }
    .topbar {
      display: flex;
      gap: 12px;
      align-items: center;
      justify-content: space-between;
      border: 1px solid var(--line);
      border-radius: 12px;
      background: color-mix(in oklab, var(--card) 88%, black 12%);
      box-shadow: var(--shadow);
      padding: 12px 14px;
    }
    .title h1 {
      margin: 0;
      font-size: 18px;
      line-height: 1.2;
      letter-spacing: -0.01em;
    }
    .title p { margin: 2px 0 0; font-size: 12px; color: var(--muted); }
    .actions {
      display: flex;
      align-items: center;
      gap: 8px;
      flex-wrap: wrap;
    }
    .pill {
      border: 1px solid color-mix(in oklab, var(--primary) 34%, var(--line));
      border-radius: 999px;
      padding: 5px 10px;
      font-size: 11px;
      font-weight: 600;
      color: color-mix(in oklab, var(--primary) 70%, white 30%);
      background: color-mix(in oklab, var(--primary) 12%, transparent);
      letter-spacing: 0.03em;
      text-transform: uppercase;
    }
    .status {
      min-height: 36px;
      border-radius: 10px;
      border: 1px solid color-mix(in oklab, var(--primary) 30%, var(--line));
      background: color-mix(in oklab, var(--primary) 12%, transparent);
      color: color-mix(in oklab, var(--primary) 78%, white 22%);
      padding: 8px 11px;
      font-size: 13px;
      display: flex;
      align-items: center;
    }
    .status.warn {
      border-color: color-mix(in oklab, var(--warning) 35%, var(--line));
      background: color-mix(in oklab, var(--warning) 14%, transparent);
      color: color-mix(in oklab, var(--warning) 78%, white 22%);
    }
    .status.err {
      border-color: color-mix(in oklab, var(--danger) 35%, var(--line));
      background: color-mix(in oklab, var(--danger) 14%, transparent);
      color: color-mix(in oklab, var(--danger) 80%, white 20%);
    }
    .stats {
      display: grid;
      gap: 10px;
      grid-template-columns: repeat(4, minmax(0, 1fr));
    }
    .stat-card {
      border-radius: 11px;
      border: 1px solid var(--line);
      background: color-mix(in oklab, var(--card) 86%, black 14%);
      padding: 12px;
      display: grid;
      gap: 8px;
      animation: rise .2s ease;
    }
    .stat-kicker {
      display: flex;
      align-items: center;
      justify-content: space-between;
      font-size: 11px;
      color: var(--muted);
    }
    .stat-kicker span:last-child {
      border: 1px solid color-mix(in oklab, var(--primary) 28%, var(--line));
      background: color-mix(in oklab, var(--primary) 11%, transparent);
      color: color-mix(in oklab, var(--primary) 70%, white 30%);
      font-size: 10px;
      border-radius: 999px;
      padding: 2px 7px;
      font-weight: 600;
    }
    .stat-value {
      font-size: 24px;
      letter-spacing: -0.02em;
      line-height: 1;
      font-weight: 600;
    }
    .stat-note {
      font-size: 11px;
      color: var(--muted);
    }
    .graph-grid {
      display: grid;
      gap: 12px;
      grid-template-columns: minmax(0, 1.6fr) minmax(0, 1fr);
      min-width: 0;
      align-items: stretch;
    }
    .graph-shell {
      border-radius: 11px;
      border: 1px solid var(--line);
      background: color-mix(in oklab, var(--card) 88%, black 12%);
      box-shadow: 0 12px 22px -18px rgba(0, 0, 0, 0.8);
      min-width: 0;
      animation: rise .2s ease;
    }
    .graph-head {
      padding: 12px 14px;
      border-bottom: 1px solid var(--line);
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 10px;
    }
    .graph-head h3 {
      margin: 0;
      font-size: 13px;
      font-weight: 600;
    }
    .graph-tag {
      border-radius: 999px;
      border: 1px solid var(--line);
      background: color-mix(in oklab, var(--secondary) 86%, black 14%);
      color: var(--muted);
      font-size: 10px;
      padding: 3px 8px;
      font-weight: 600;
      letter-spacing: .02em;
    }
    .graph-body {
      padding: 12px 14px;
      min-width: 0;
    }
    .traffic-placeholder {
      position: relative;
      height: 210px;
      border: 1px solid var(--line);
      border-radius: 10px;
      overflow: hidden;
      background:
        linear-gradient(color-mix(in oklab, var(--line) 55%, transparent) 1px, transparent 1px),
        linear-gradient(90deg, color-mix(in oklab, var(--line) 40%, transparent) 1px, transparent 1px),
        color-mix(in oklab, var(--sidebar) 65%, black 35%);
      background-size: 100% 36px, 40px 100%, auto;
    }
    .traffic-wave {
      position: absolute;
      inset: auto 0 0;
      height: 65%;
      background:
        radial-gradient(140% 85% at 8% 100%, color-mix(in oklab, var(--primary) 34%, transparent) 0 30%, transparent 32%),
        radial-gradient(130% 90% at 32% 100%, color-mix(in oklab, var(--chart-2) 26%, transparent) 0 30%, transparent 32%),
        radial-gradient(130% 85% at 52% 100%, color-mix(in oklab, var(--primary) 30%, transparent) 0 28%, transparent 31%),
        radial-gradient(110% 95% at 74% 100%, color-mix(in oklab, var(--chart-2) 22%, transparent) 0 29%, transparent 33%),
        radial-gradient(120% 100% at 94% 100%, color-mix(in oklab, var(--primary) 26%, transparent) 0 27%, transparent 31%);
      opacity: .95;
    }
    .traffic-line {
      position: absolute;
      left: 14px;
      right: 14px;
      bottom: 18px;
      height: 120px;
      border-radius: 8px;
      border: 1px dashed color-mix(in oklab, var(--primary) 46%, transparent);
      background:
        linear-gradient(120deg,
          transparent 0% 8%,
          color-mix(in oklab, var(--primary) 72%, transparent) 8% 10%,
          transparent 10% 21%,
          color-mix(in oklab, var(--primary) 72%, transparent) 21% 23%,
          transparent 23% 38%,
          color-mix(in oklab, var(--primary) 72%, transparent) 38% 40%,
          transparent 40% 58%,
          color-mix(in oklab, var(--primary) 72%, transparent) 58% 60%,
          transparent 60% 78%,
          color-mix(in oklab, var(--primary) 72%, transparent) 78% 80%,
          transparent 80% 100%);
      opacity: .52;
    }
    .graph-note {
      margin-top: 9px;
      font-size: 11px;
      color: var(--muted);
    }
    .gauge-grid {
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 10px;
    }
    .gauge {
      border: 1px solid var(--line);
      border-radius: 10px;
      background: color-mix(in oklab, var(--secondary) 82%, black 18%);
      padding: 10px;
      display: grid;
      grid-template-columns: auto 1fr;
      gap: 10px;
      align-items: center;
    }
    .gauge-ring {
      width: 52px;
      height: 52px;
      border-radius: 999px;
      background:
        conic-gradient(
          color-mix(in oklab, var(--primary) 80%, white 20%) 0 230deg,
          color-mix(in oklab, var(--line) 85%, black 15%) 230deg 360deg
        );
      position: relative;
    }
    .gauge-ring::after {
      content: "";
      position: absolute;
      inset: 7px;
      border-radius: inherit;
      background: color-mix(in oklab, var(--sidebar) 75%, black 25%);
      border: 1px solid var(--line);
    }
    .gauge:nth-child(2) .gauge-ring {
      background: conic-gradient(color-mix(in oklab, var(--chart-2) 80%, white 20%) 0 260deg, color-mix(in oklab, var(--line) 85%, black 15%) 260deg 360deg);
    }
    .gauge:nth-child(3) .gauge-ring {
      background: conic-gradient(color-mix(in oklab, var(--chart-3) 80%, white 20%) 0 190deg, color-mix(in oklab, var(--line) 85%, black 15%) 190deg 360deg);
    }
    .gauge:nth-child(4) .gauge-ring {
      background: conic-gradient(color-mix(in oklab, var(--warning) 85%, white 15%) 0 160deg, color-mix(in oklab, var(--line) 85%, black 15%) 160deg 360deg);
    }
    .gauge strong {
      display: block;
      font-size: 12px;
      line-height: 1.2;
    }
    .gauge span {
      display: block;
      font-size: 11px;
      color: var(--muted);
      margin-top: 2px;
    }
    .layout {
      display: grid;
      gap: 12px;
      grid-template-columns: minmax(0, 1.6fr) minmax(0, 1fr);
      align-items: start;
      min-width: 0;
    }
    .stack {
      display: grid;
      gap: 12px;
      align-content: start;
      min-width: 0;
    }
    .card {
      border-radius: 11px;
      border: 1px solid var(--line);
      background: color-mix(in oklab, var(--card) 88%, black 12%);
      box-shadow: 0 12px 22px -18px rgba(0, 0, 0, 0.8);
      min-width: 0;
      animation: rise .2s ease;
    }
    .card-head {
      padding: 12px 14px;
      border-bottom: 1px solid var(--line);
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 10px;
    }
    .card-head h3 {
      margin: 0;
      font-size: 13px;
      font-weight: 600;
      letter-spacing: 0;
      text-transform: none;
    }
    .card-body {
      padding: 12px 14px;
      min-width: 0;
    }
    .card-body.collapsed {
      display: none;
    }
    .collapse-btn {
      min-width: 86px;
      text-align: center;
    }
    .row {
      display: grid;
      gap: 8px;
      grid-template-columns: 1fr;
    }
    .triple {
      display: grid;
      gap: 8px;
      grid-template-columns: 1fr 1fr 1fr;
    }
    input {
      width: 100%;
      padding: 9px 10px;
      border: 1px solid var(--line);
      border-radius: 9px;
      background: color-mix(in oklab, var(--secondary) 90%, black 10%);
      color: var(--foreground);
      font: inherit;
      font-size: 13px;
      transition: border-color .12s ease, box-shadow .12s ease;
    }
    input::placeholder { color: color-mix(in oklab, var(--muted) 84%, white 16%); }
    input:focus {
      outline: none;
      border-color: color-mix(in oklab, var(--primary) 50%, var(--line));
      box-shadow: 0 0 0 3px color-mix(in oklab, var(--primary) 16%, transparent);
    }
    button {
      border: 0;
      border-radius: 9px;
      padding: 9px 11px;
      font: inherit;
      font-size: 12px;
      font-weight: 600;
      color: #07170f;
      background: color-mix(in oklab, var(--primary) 88%, white 12%);
      cursor: pointer;
      white-space: nowrap;
      transition: filter .16s ease, transform .08s ease;
      box-shadow: none;
    }
    button:hover { filter: brightness(1.04); }
    button:active { transform: translateY(1px); }
    button.secondary {
      background: color-mix(in oklab, var(--secondary) 88%, black 12%);
      color: color-mix(in oklab, var(--muted) 75%, white 25%);
      border: 1px solid var(--line);
      font-weight: 500;
    }
    .list {
      list-style: none;
      margin: 0;
      padding: 0;
      display: grid;
      gap: 7px;
    }
    .item-row {
      border: 1px solid var(--line);
      border-radius: 10px;
      background: color-mix(in oklab, var(--secondary) 78%, black 22%);
      padding: 9px 10px;
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 10px;
      align-items: start;
      min-width: 0;
    }
    .item-row.selected {
      border-color: color-mix(in oklab, var(--primary) 42%, var(--line));
      background: color-mix(in oklab, var(--primary) 12%, transparent);
    }
    .item-main {
      display: grid;
      gap: 2px;
      min-width: 0;
      width: 100%;
    }
    .item-main strong {
      font-size: 12px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .item-main code {
      display: block;
      font-family: 'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
      font-size: 11px;
      color: var(--muted);
      white-space: normal;
      overflow-wrap: break-word;
    }
    .item-actions {
      display: flex;
      gap: 6px;
      flex-wrap: wrap;
      justify-content: flex-end;
      align-items: flex-start;
    }
    .item-actions button {
      padding: 6px 8px;
      font-size: 11px;
    }
    .pill-status {
      border-radius: 999px;
      font-size: 10px;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: .04em;
      padding: 4px 8px;
      border: 1px solid color-mix(in oklab, var(--ok) 36%, var(--line));
      background: color-mix(in oklab, var(--ok) 12%, transparent);
      color: color-mix(in oklab, var(--ok) 80%, white 20%);
    }
    .pill-status.status-queued,
    .pill-status.status-deploying,
    .pill-status.status-retrying {
      border-color: color-mix(in oklab, var(--warning) 40%, var(--line));
      background: color-mix(in oklab, var(--warning) 14%, transparent);
      color: color-mix(in oklab, var(--warning) 80%, white 20%);
    }
    .pill-status.status-healthy {
      border-color: color-mix(in oklab, var(--ok) 36%, var(--line));
      background: color-mix(in oklab, var(--ok) 12%, transparent);
      color: color-mix(in oklab, var(--ok) 80%, white 20%);
    }
    .pill-status.status-failed {
      border-color: color-mix(in oklab, var(--danger) 42%, var(--line));
      background: color-mix(in oklab, var(--danger) 15%, transparent);
      color: color-mix(in oklab, var(--danger) 82%, white 18%);
    }
    .item-row.in-progress {
      border-color: color-mix(in oklab, var(--warning) 36%, var(--line));
      background: color-mix(in oklab, var(--warning) 10%, transparent);
    }
    pre {
      margin: 0;
      min-height: 260px;
      max-height: 460px;
      overflow: auto;
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      border-radius: 10px;
      background: color-mix(in oklab, var(--sidebar) 70%, black 30%);
      color: color-mix(in oklab, var(--foreground) 92%, white 8%);
      font-family: 'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
      font-size: 12px;
      line-height: 1.45;
      border: 1px solid var(--line);
      padding: 11px;
    }
    .hint {
      color: var(--muted);
      font-size: 12px;
      margin-top: 8px;
    }
    @keyframes rise {
      from { opacity: 0; transform: translateY(5px); }
      to { opacity: 1; transform: translateY(0); }
    }
    @media (max-width: 1120px) {
      .stats { grid-template-columns: 1fr 1fr; }
      .graph-grid { grid-template-columns: 1fr; }
      .layout { grid-template-columns: 1fr; }
      .triple { grid-template-columns: 1fr; }
    }
    @media (max-width: 920px) {
      .shell { grid-template-columns: 1fr; }
      .sidebar {
        position: static;
        min-height: auto;
      }
      .side-group { grid-template-columns: 1fr 1fr; }
      .sidebar-bottom { margin-top: 2px; }
    }
    @media (max-width: 640px) {
      .main { padding: 12px; }
      .topbar {
        flex-direction: column;
        align-items: stretch;
      }
      .actions { justify-content: space-between; }
      .stats { grid-template-columns: 1fr; }
      .gauge-grid { grid-template-columns: 1fr; }
      .side-group { grid-template-columns: 1fr; }
    }
  </style>
</head>
<body>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand-block">
        <div class="brand-icon">R</div>
        <div>
          <p class="brand-title">Rustploy</p>
          <p class="brand-sub">Control Plane</p>
        </div>
      </div>

      <div class="search">
        <span>Search...</span>
        <code>/</code>
      </div>

      <div class="side-group">
        <button class="side-item active">Dashboard <span class="side-count">01</span></button>
        <button class="side-item"><strong>Projects</strong> <span class="side-count">12</span></button>
        <button class="side-item"><strong>Domains</strong></button>
        <button class="side-item"><strong>Deployments</strong></button>
        <button class="side-item" onclick="window.location.href='/logs'"><strong>Logs Explorer</strong></button>
      </div>

      <div class="side-group">
        <div class="side-label">Infrastructure</div>
        <button class="side-item"><strong>Servers</strong></button>
        <button class="side-item"><strong>Certificates</strong></button>
        <button class="side-item"><strong>Team</strong></button>
      </div>

      <div class="sidebar-bottom">
        <button class="new-project">+ New Project</button>
        <div class="profile">
          <strong>Owner Session</strong>
          <span>admin@localhost</span>
        </div>
      </div>
    </aside>

    <main class="main">
      <header class="topbar">
        <div class="title">
          <h1>Dashboard</h1>
          <p>Git-driven deploy control for small-node runtimes.</p>
        </div>
        <div class="actions">
          <button class="secondary" onclick="window.location.href='/logs'">Logs Page</button>
          <span class="pill" id="active-app-pill">No app selected</span>
          <button class="secondary" onclick="logout()">Logout</button>
        </div>
      </header>

      <div id="status" class="status">Ready.</div>

      <section class="stats">
        <article class="stat-card">
          <div class="stat-kicker"><span>Applications</span><span>Live</span></div>
          <div class="stat-value">12</div>
          <div class="stat-note">Deployments managed from one panel</div>
        </article>
        <article class="stat-card">
          <div class="stat-kicker"><span>Databases</span><span>Healthy</span></div>
          <div class="stat-value">4</div>
          <div class="stat-note">Managed backing services online</div>
        </article>
        <article class="stat-card">
          <div class="stat-kicker"><span>Domains</span><span>SSL</span></div>
          <div class="stat-value">18</div>
          <div class="stat-note">Routed endpoints with managed TLS</div>
        </article>
        <article class="stat-card">
          <div class="stat-kicker"><span>Uptime</span><span>30d</span></div>
          <div class="stat-value">99.97%</div>
          <div class="stat-note">Runtime availability trend</div>
        </article>
      </section>

      <section class="graph-grid">
        <article class="graph-shell">
          <div class="graph-head">
            <h3>Request Traffic</h3>
            <span class="graph-tag">Placeholder</span>
          </div>
          <div class="graph-body">
            <div class="traffic-placeholder">
              <div class="traffic-wave"></div>
              <div class="traffic-line"></div>
            </div>
            <p class="graph-note">Chart placeholder for request/error timeseries.</p>
          </div>
        </article>

        <article class="graph-shell">
          <div class="graph-head">
            <h3>Server Resources</h3>
            <span class="graph-tag">Placeholder</span>
          </div>
          <div class="graph-body">
            <div class="gauge-grid">
              <div class="gauge">
                <div class="gauge-ring"></div>
                <div><strong>CPU</strong><span>graph placeholder</span></div>
              </div>
              <div class="gauge">
                <div class="gauge-ring"></div>
                <div><strong>Memory</strong><span>graph placeholder</span></div>
              </div>
              <div class="gauge">
                <div class="gauge-ring"></div>
                <div><strong>Disk</strong><span>graph placeholder</span></div>
              </div>
              <div class="gauge">
                <div class="gauge-ring"></div>
                <div><strong>Network</strong><span>graph placeholder</span></div>
              </div>
            </div>
            <p class="graph-note">Resource gauges are placeholders until live metrics wiring lands.</p>
          </div>
        </article>
      </section>

      <section class="layout">
        <div class="stack">
          <article class="card">
            <div class="card-head">
              <h3>Projects</h3>
            </div>
            <div class="card-body">
              <ul id="apps" class="list"></ul>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Recent Deployments</h3>
              <button
                class="secondary collapse-btn"
                id="deployments-collapse-btn"
                onclick="toggleDeploymentsCard()"
              >
                Collapse
              </button>
            </div>
            <div class="card-body" id="deployments-card-body">
              <ul id="deployments" class="list"></ul>
              <p class="hint">Stream updates enabled for selected app.</p>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Live Logs</h3>
            </div>
            <div class="card-body">
              <pre id="logs">(no logs yet)</pre>
            </div>
          </article>
        </div>

        <div class="stack">
          <article class="card">
            <div class="card-head">
              <h3>Create App</h3>
            </div>
            <div class="card-body">
              <div class="row">
                <input id="app-name" placeholder="my-app" />
                <button onclick="createApp()">Create App</button>
              </div>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Import GitHub Repo</h3>
            </div>
            <div class="card-body">
              <div class="triple">
                <input id="owner" placeholder="owner" />
                <input id="repo" placeholder="repo" />
                <input id="branch" placeholder="main" value="main" />
              </div>
              <div class="row" style="margin-top:8px">
                <button onclick="importApp()">Import Repository</button>
              </div>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Routing & Runtime</h3>
            </div>
            <div class="card-body">
              <ul id="routes" class="list"></ul>
              <p class="hint" id="runtime-note">
                Latest healthy compose deployments are routed to live runtime containers.
              </p>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Selected Deployment</h3>
            </div>
            <div class="card-body">
              <ul id="selected-deployment-summary" class="list"></ul>
              <p class="hint">Shows condensed details for the selected deployment entry.</p>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Domains</h3>
            </div>
            <div class="card-body">
              <div class="row">
                <input id="domain" placeholder="app.example.com" />
                <button onclick="addDomain()">Add Domain</button>
              </div>
              <ul id="domains" class="list" style="margin-top:10px"></ul>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Environment</h3>
            </div>
            <div class="card-body">
              <div class="row">
                <input id="env-key" placeholder="DATABASE_URL" />
                <input id="env-value" placeholder="value" />
                <button onclick="setEnvVar()">Set</button>
              </div>
              <ul id="env-vars" class="list" style="margin-top:10px"></ul>
              <p class="hint">Applied to the app service during compose deployments.</p>
            </div>
          </article>
        </div>
      </section>
    </main>
  </div>
  <script>
    let selectedApp = null;
    let selectedAppName = null;
    let selectedLogsDeploymentId = null;
    let logsStream = null;
    let statusTimeout = null;
    const selectedState = {
      domains: [],
      envVars: [],
      latestDeployment: null,
      selectedDeployment: null,
      deployments: [],
      config: null
    };
    const pendingDeploymentsByApp = new Map();
    const ACTIVE_DEPLOYMENT_STATUSES = new Set(['queued', 'deploying', 'retrying']);
    let deploymentsCollapsed = false;

    function normalizeDeploymentStatus(value) {
      return (value || 'queued').toString().toLowerCase();
    }

    function isActiveDeploymentStatus(value) {
      return ACTIVE_DEPLOYMENT_STATUSES.has(normalizeDeploymentStatus(value));
    }

    function deploymentPillClass(value) {
      const status = normalizeDeploymentStatus(value);
      if (status === 'healthy') return 'status-healthy';
      if (status === 'failed') return 'status-failed';
      if (status === 'deploying') return 'status-deploying';
      if (status === 'retrying') return 'status-retrying';
      return 'status-queued';
    }

    function formatUnixMs(value) {
      if (typeof value !== 'number' || Number.isNaN(value) || value <= 0) {
        return 'unknown';
      }
      try {
        return new Date(value).toLocaleString();
      } catch (_) {
        return 'unknown';
      }
    }

    function applyDeploymentsCollapsed() {
      const body = document.getElementById('deployments-card-body');
      const button = document.getElementById('deployments-collapse-btn');
      if (!body || !button) return;
      body.classList.toggle('collapsed', deploymentsCollapsed);
      button.textContent = deploymentsCollapsed ? 'Expand' : 'Collapse';
    }

    function toggleDeploymentsCard() {
      deploymentsCollapsed = !deploymentsCollapsed;
      applyDeploymentsCollapsed();
    }

    function renderSelectedDeployment() {
      const el = document.getElementById('selected-deployment-summary');
      if (!el) return;
      el.innerHTML = '';

      if (!selectedApp) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'Select an app to inspect deployment details.';
        el.appendChild(empty);
        return;
      }

      const deployment = selectedState.selectedDeployment;
      if (!deployment) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'No deployment selected yet.';
        el.appendChild(empty);
        return;
      }

      const status = normalizeDeploymentStatus(deployment.status);
      const sourceRef = deployment.source_ref || 'manual';
      const commitSha = deployment.commit_sha || 'n/a';
      const imageRef = deployment.image_ref || 'n/a';
      const updatedAt = formatUnixMs(deployment.updated_at_unix_ms);

      const rowTop = document.createElement('li');
      rowTop.className = `item-row ${isActiveDeploymentStatus(status) ? 'in-progress' : ''}`.trim();
      const topMain = document.createElement('div');
      topMain.className = 'item-main';
      const topStrong = document.createElement('strong');
      const topPill = document.createElement('span');
      topPill.className = `pill-status ${deploymentPillClass(status)}`;
      topPill.textContent = status;
      topStrong.appendChild(topPill);
      const topCode = document.createElement('code');
      topCode.textContent = deployment.id;
      topMain.appendChild(topStrong);
      topMain.appendChild(topCode);
      rowTop.appendChild(topMain);
      el.appendChild(rowTop);

      const rowDetails = document.createElement('li');
      rowDetails.className = 'item-row';
      const detailMain = document.createElement('div');
      detailMain.className = 'item-main';
      const detailStrong = document.createElement('strong');
      detailStrong.textContent = selectedAppName || 'Selected app';
      const sourceCode = document.createElement('code');
      sourceCode.textContent = `source=${sourceRef}`;
      const commitCode = document.createElement('code');
      commitCode.textContent = `commit=${commitSha}`;
      const imageCode = document.createElement('code');
      imageCode.textContent = `image=${imageRef}`;
      const updatedCode = document.createElement('code');
      updatedCode.textContent = `updated=${updatedAt}`;
      detailMain.appendChild(detailStrong);
      detailMain.appendChild(sourceCode);
      detailMain.appendChild(commitCode);
      detailMain.appendChild(imageCode);
      detailMain.appendChild(updatedCode);
      rowDetails.appendChild(detailMain);
      el.appendChild(rowDetails);
    }

    function renderDashboardDeploymentsList(items) {
      const el = document.getElementById('deployments');
      if (!el) return;
      el.innerHTML = '';

      if (!items || items.length === 0) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'No deployments yet.';
        el.appendChild(empty);
        return;
      }

      for (const deployment of items) {
        const sourceRef = deployment.source_ref || 'manual';
        const status = normalizeDeploymentStatus(deployment.status);
        const building = isActiveDeploymentStatus(status);
        const isSelected =
          selectedState.selectedDeployment && selectedState.selectedDeployment.id === deployment.id;
        const statusLabel = building ? `${status} (building)` : status;

        const item = document.createElement('li');
        item.className = `item-row ${building ? 'in-progress' : ''} ${isSelected ? 'selected' : ''}`.trim();

        const main = document.createElement('div');
        main.className = 'item-main';
        const strong = document.createElement('strong');
        const pill = document.createElement('span');
        pill.className = `pill-status ${deploymentPillClass(status)}`;
        pill.textContent = statusLabel;
        strong.appendChild(pill);
        const code = document.createElement('code');
        code.textContent = sourceRef;
        main.appendChild(strong);
        main.appendChild(code);

        const actions = document.createElement('div');
        actions.className = 'item-actions';

        const selectButton = document.createElement('button');
        selectButton.className = 'secondary';
        selectButton.textContent = isSelected ? 'Selected' : 'Select';
        selectButton.onclick = () => selectDeployment(deployment.id);

        const logsButton = document.createElement('button');
        logsButton.className = 'secondary';
        logsButton.textContent = 'Logs';
        logsButton.onclick = () => showLogs(selectedApp, deployment.id);

        actions.appendChild(selectButton);
        actions.appendChild(logsButton);

        item.appendChild(main);
        item.appendChild(actions);
        el.appendChild(item);
      }
    }

    function selectDeployment(deploymentId) {
      const found = selectedState.deployments.find((item) => item.id === deploymentId);
      if (!found) return;
      selectedState.selectedDeployment = found;
      renderSelectedDeployment();
      renderDashboardDeploymentsList(selectedState.deployments);
    }

    function markPendingDeployment(appId, deploymentId, status, sourceRef) {
      if (!appId || !deploymentId) return;
      const now = Date.now();
      pendingDeploymentsByApp.set(appId, {
        id: deploymentId,
        app_id: appId,
        status: normalizeDeploymentStatus(status),
        source_ref: sourceRef || 'manual',
        image_ref: null,
        commit_sha: null,
        last_error: null,
        created_at_unix_ms: now,
        updated_at_unix_ms: now
      });
    }

    function mergePendingDeployment(appId, items) {
      const pending = pendingDeploymentsByApp.get(appId);
      if (!pending) return items;
      const hasPending = items.some((item) => item.id === pending.id);
      if (hasPending) {
        pendingDeploymentsByApp.delete(appId);
        return items;
      }
      return [pending, ...items];
    }

    function setStatus(message, level = 'ok') {
      const el = document.getElementById('status');
      el.textContent = message;
      el.className = 'status';
      if (level === 'warn') el.classList.add('warn');
      if (level === 'err') el.classList.add('err');
      if (statusTimeout) clearTimeout(statusTimeout);
      statusTimeout = setTimeout(() => {
        el.textContent = 'Ready.';
        el.className = 'status';
      }, 4500);
    }

    function setSelectedPill(appName) {
      document.getElementById('active-app-pill').textContent = appName
        ? `Active: ${appName}`
        : 'No app selected';
    }

    function appLocalhostHost(appName) {
      if (!appName) return null;
      const slug = appName
        .toLowerCase()
        .replace(/[^a-z0-9-]/g, '-')
        .replace(/-+/g, '-')
        .replace(/^-|-$/g, '');
      return slug ? `${slug}.localhost` : null;
    }

    function isControlPlaneDomain(domain) {
      const normalized = (domain || '').trim().toLowerCase().replace(/\.$/, '');
      return normalized === 'localhost'
        || normalized === 'rustploy.localhost'
        || normalized === '127.0.0.1'
        || normalized === '::1';
    }

    function renderRoutes() {
      const routes = document.getElementById('routes');
      const note = document.getElementById('runtime-note');
      routes.innerHTML = '';

      if (!selectedApp) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'Select an app to inspect routing details.';
        routes.appendChild(empty);
        note.textContent =
          'Latest healthy compose deployments are routed to live runtime containers.';
        return;
      }

      const appRow = document.createElement('li');
      appRow.className = 'item-row';
      appRow.innerHTML = `
        <div class="item-main">
          <strong>${selectedAppName || 'Selected app'}</strong>
          <code>${selectedApp}</code>
        </div>
      `;
      routes.appendChild(appRow);

      const appHost = appLocalhostHost(selectedAppName);
      if (appHost) {
        const hostRow = document.createElement('li');
        hostRow.className = 'item-row';
        hostRow.innerHTML = `
          <div class="item-main">
            <strong>Default Dev URL</strong>
            <code>https://${appHost}</code>
          </div>
        `;
        routes.appendChild(hostRow);
      }

      const deploymentRow = document.createElement('li');
      deploymentRow.className = 'item-row';
      if (selectedState.latestDeployment) {
        const sourceRef = selectedState.latestDeployment.source_ref || 'manual';
        const status = normalizeDeploymentStatus(selectedState.latestDeployment.status);
        const building = isActiveDeploymentStatus(status);
        const statusLabel = building ? `${status} (building)` : status;
        deploymentRow.className = `item-row ${building ? 'in-progress' : ''}`.trim();
        deploymentRow.innerHTML = `
          <div class="item-main">
            <strong><span class="pill-status ${deploymentPillClass(status)}">${statusLabel}</span></strong>
            <code>${sourceRef}</code>
          </div>
        `;
      } else {
        deploymentRow.innerHTML = `
          <div class="item-main">
            <strong>Deployment status</strong>
            <code>none</code>
          </div>
        `;
      }
      routes.appendChild(deploymentRow);

      const routedDomains = selectedState.domains.filter((domain) =>
        !isControlPlaneDomain(domain.domain)
      );

      if (routedDomains.length === 0) {
        const emptyDomain = document.createElement('li');
        emptyDomain.className = 'item-row';
        emptyDomain.innerHTML = `
          <div class="item-main">
            <strong>Public URL</strong>
            <code>no domain mapped</code>
          </div>
        `;
        routes.appendChild(emptyDomain);
      } else {
        for (const domain of routedDomains) {
          const domainRow = document.createElement('li');
          domainRow.className = 'item-row';
          domainRow.innerHTML = `
            <div class="item-main">
              <strong>https://${domain.domain}</strong>
              <code>tls=${domain.tls_mode}</code>
            </div>
          `;
          routes.appendChild(domainRow);
        }
      }

      if (selectedState.config) {
        const cfg = selectedState.config;
        const cfgRow = document.createElement('li');
        cfgRow.className = 'item-row';
        cfgRow.innerHTML = `
          <div class="item-main">
            <strong>${cfg.detection.framework} (${cfg.detection.package_manager})</strong>
            <code>${cfg.detection.build_profile}</code>
          </div>
        `;
        routes.appendChild(cfgRow);

        if (cfg.compose) {
          const composeRow = document.createElement('li');
          const appService = cfg.compose.app_service || 'unknown';
          composeRow.className = 'item-row';
          composeRow.innerHTML = `
            <div class="item-main">
              <strong>compose: ${appService}</strong>
              <code>${cfg.compose.file} (${cfg.compose.services.length} services)</code>
            </div>
          `;
          routes.appendChild(composeRow);
        }
      }

      if (selectedState.latestDeployment && isActiveDeploymentStatus(selectedState.latestDeployment.status)) {
        note.textContent =
          'Build in progress. Status auto-refreshes every few seconds; logs stream live below.';
      } else {
        note.textContent =
          'Use the app localhost URL above to reach the latest healthy deployment. Compose apps are now routed to live runtime containers.';
      }
    }

    async function api(path, opts = {}) {
      const res = await fetch(path, { credentials: 'include', ...opts });
      if (res.status === 401) {
        window.location.reload();
        throw new Error('unauthorized');
      }
      if (!res.ok) {
        const text = await res.text();
        throw new Error(text || `request failed: ${res.status}`);
      }
      return res;
    }

    async function refreshApps() {
      const res = await api('/api/v1/apps');
      const data = await res.json();
      const apps = document.getElementById('apps');
      apps.innerHTML = '';

      if (data.items.length === 0) {
        selectedApp = null;
        selectedAppName = null;
        selectedState.domains = [];
        selectedState.envVars = [];
        selectedState.latestDeployment = null;
        selectedState.selectedDeployment = null;
        selectedState.deployments = [];
        selectedState.config = null;
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'No apps yet.';
        apps.appendChild(empty);
        setSelectedPill(null);
        renderRoutes();
        renderSelectedDeployment();
        document.getElementById('env-vars').innerHTML = '<li class="hint">Select an app.</li>';
        return;
      }

      let selectedStillExists = false;
      for (const app of data.items) {
        if (selectedApp === app.id) {
          selectedStillExists = true;
          selectedAppName = app.name;
        }
        const li = document.createElement('li');
        li.className = `item-row ${selectedApp === app.id ? 'selected' : ''}`.trim();
        li.innerHTML = `
          <div class="item-main">
            <strong>${app.name}</strong>
            <code>${app.id}</code>
          </div>
          <div class="item-actions">
            <button class="secondary" onclick="selectApp('${app.id}','${app.name}')">Open</button>
            <button onclick="deploy('${app.id}')">Deploy</button>
            <button class="secondary" onclick="resyncRebuild('${app.id}')">Resync & Rebuild</button>
            <button class="secondary" onclick="rollback('${app.id}')">Rollback</button>
          </div>
        `;
        apps.appendChild(li);
      }

      if (selectedStillExists) {
        setSelectedPill(selectedAppName);
      }

      if (!selectedApp && data.items.length > 0) {
        await selectApp(data.items[0].id, data.items[0].name);
      } else if (!selectedStillExists) {
        selectedApp = null;
        selectedAppName = null;
        selectedState.domains = [];
        selectedState.envVars = [];
        selectedState.latestDeployment = null;
        selectedState.selectedDeployment = null;
        selectedState.deployments = [];
        selectedState.config = null;
        setSelectedPill(null);
        renderRoutes();
        renderSelectedDeployment();
        document.getElementById('env-vars').innerHTML = '<li class="hint">Select an app.</li>';
      }
    }

    async function createApp() {
      const name = document.getElementById('app-name').value.trim();
      if (!name) {
        setStatus('App name is required.', 'warn');
        return;
      }
      try {
        await api('/api/v1/apps', {
          method: 'POST',
          headers: {'content-type': 'application/json'},
          body: JSON.stringify({name})
        });
        document.getElementById('app-name').value = '';
        setStatus(`Created app "${name}".`);
        await refreshApps();
      } catch (error) {
        setStatus(`Create app failed: ${error.message}`, 'err');
      }
    }

    async function importApp() {
      const owner = document.getElementById('owner').value.trim();
      const repo = document.getElementById('repo').value.trim();
      const branch = document.getElementById('branch').value.trim() || 'main';
      if (!owner || !repo) {
        setStatus('Owner and repo are required for import.', 'warn');
        return;
      }
      try {
        await api('/api/v1/apps/import', {
          method: 'POST',
          headers: {'content-type': 'application/json'},
          body: JSON.stringify({
            repository: { provider:'github', owner, name:repo, default_branch:branch },
            source: { branch }
          })
        });
        setStatus(`Imported ${owner}/${repo}@${branch}.`);
        await refreshApps();
      } catch (error) {
        setStatus(`Import failed: ${error.message}`, 'err');
      }
    }

    async function selectApp(appId, appName = null) {
      selectedApp = appId;
      selectedAppName = appName || selectedAppName;
      selectedLogsDeploymentId = null;
      selectedState.domains = [];
      selectedState.envVars = [];
      selectedState.latestDeployment = null;
      selectedState.selectedDeployment = null;
      selectedState.deployments = [];
      selectedState.config = null;
      setSelectedPill(selectedAppName);
      renderRoutes();
      renderSelectedDeployment();
      await refreshDeployments();
      await refreshDomains();
      await refreshEnvVars();
      await refreshAppConfig();
      await refreshApps();
      startLogsStream();
    }

    function startLogsStream() {
      if (logsStream) {
        logsStream.close();
        logsStream = null;
      }
      if (!selectedApp) return;
      selectedLogsDeploymentId = null;
      document.getElementById('logs').textContent = '(waiting for logs...)';
      logsStream = new EventSource(`/api/v1/apps/${selectedApp}/logs/stream`, { withCredentials: true });
      logsStream.addEventListener('logs', (event) => {
        let payload = null;
        try {
          payload = JSON.parse(event.data || '{}');
        } catch (_) {
          return;
        }

        const deploymentId = payload && payload.deployment_id ? payload.deployment_id : null;
        const logsChunk = payload && typeof payload.logs === 'string' ? payload.logs : '';
        const reset = payload && payload.reset === true;
        const logsEl = document.getElementById('logs');

        if (!deploymentId && !logsChunk) {
          selectedLogsDeploymentId = null;
          logsEl.textContent = '(no logs)';
          return;
        }

        if (reset || deploymentId !== selectedLogsDeploymentId) {
          selectedLogsDeploymentId = deploymentId;
          logsEl.textContent = logsChunk || '(no logs)';
          return;
        }

        if (!logsChunk) {
          return;
        }

        const current = logsEl.textContent === '(no logs)' || logsEl.textContent === '(waiting for logs...)'
          ? ''
          : logsEl.textContent;
        logsEl.textContent = current ? `${current}\n${logsChunk}` : logsChunk;
      });
      logsStream.onerror = () => {};
    }

    async function refreshDeployments() {
      if (!selectedApp) return;
      const res = await api(`/api/v1/apps/${selectedApp}/deployments`);
      const data = await res.json();
      const items = mergePendingDeployment(selectedApp, data.items || []);
      selectedState.deployments = items;
      selectedState.latestDeployment = items[0] || null;
      const selectedDeploymentId = selectedState.selectedDeployment ? selectedState.selectedDeployment.id : null;
      selectedState.selectedDeployment = items.find((item) => item.id === selectedDeploymentId) || items[0] || null;
      renderRoutes();
      renderSelectedDeployment();
      renderDashboardDeploymentsList(items);
    }

    async function showLogs(appId, deploymentId) {
      try {
        const res = await api(`/api/v1/apps/${appId}/deployments/${deploymentId}/logs`);
        const data = await res.json();
        selectedLogsDeploymentId = deploymentId;
        selectDeployment(deploymentId);
        document.getElementById('logs').textContent = data.logs || '(no logs)';
      } catch (error) {
        setStatus(`Unable to load logs: ${error.message}`, 'err');
      }
    }

    async function refreshDomains() {
      if (!selectedApp) return;
      const res = await api(`/api/v1/apps/${selectedApp}/domains`);
      const data = await res.json();
      selectedState.domains = data.items;
      renderRoutes();
      const el = document.getElementById('domains');
      el.innerHTML = '';

      const routedDomains = data.items.filter((domain) =>
        !isControlPlaneDomain(domain.domain)
      );

      if (routedDomains.length === 0) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'No domain mappings.';
        el.appendChild(empty);
        return;
      }

      for (const d of routedDomains) {
        const li = document.createElement('li');
        li.className = 'item-row';
        li.innerHTML = `
          <div class="item-main">
            <strong>${d.domain}</strong>
            <code>${d.tls_mode}</code>
          </div>
        `;
        el.appendChild(li);
      }
    }

    async function refreshEnvVars() {
      if (!selectedApp) return;
      const res = await api(`/api/v1/apps/${selectedApp}/env`);
      const data = await res.json();
      selectedState.envVars = data.items;
      const el = document.getElementById('env-vars');
      el.innerHTML = '';

      if (data.items.length === 0) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'No environment variables.';
        el.appendChild(empty);
        return;
      }

      for (const envVar of data.items) {
        const li = document.createElement('li');
        li.className = 'item-row';
        li.innerHTML = `
          <div class="item-main">
            <strong>${envVar.key}</strong>
            <code>${envVar.value}</code>
          </div>
          <div class="item-actions">
            <button class="secondary" onclick="deleteEnvVar('${encodeURIComponent(envVar.key)}')">Delete</button>
          </div>
        `;
        el.appendChild(li);
      }
    }

    async function refreshAppConfig() {
      if (!selectedApp) return;
      try {
        const res = await api(`/api/v1/apps/${selectedApp}/config`);
        selectedState.config = await res.json();
      } catch (error) {
        if ((error.message || '').includes('404')) {
          selectedState.config = null;
        } else {
          setStatus(`Unable to load app config: ${error.message}`, 'warn');
        }
      }
      renderRoutes();
    }

    async function setEnvVar() {
      if (!selectedApp) {
        setStatus('Select an app before editing environment variables.', 'warn');
        return;
      }
      const key = document.getElementById('env-key').value.trim();
      const value = document.getElementById('env-value').value;
      if (!key) {
        setStatus('Environment variable key is required.', 'warn');
        return;
      }
      try {
        await api(`/api/v1/apps/${selectedApp}/env`, {
          method:'PUT',
          headers:{'content-type':'application/json'},
          body:JSON.stringify({ key, value })
        });
        document.getElementById('env-key').value = '';
        document.getElementById('env-value').value = '';
        setStatus(`Environment variable ${key} saved.`);
        await refreshEnvVars();
      } catch (error) {
        setStatus(`Set env var failed: ${error.message}`, 'err');
      }
    }

    async function deleteEnvVar(encodedKey) {
      if (!selectedApp) return;
      const key = decodeURIComponent(encodedKey);
      try {
        await api(`/api/v1/apps/${selectedApp}/env/${encodedKey}`, {
          method:'DELETE'
        });
        setStatus(`Environment variable ${key} deleted.`);
        await refreshEnvVars();
      } catch (error) {
        setStatus(`Delete env var failed: ${error.message}`, 'err');
      }
    }

    async function addDomain() {
      if (!selectedApp) {
        setStatus('Select an app before adding domains.', 'warn');
        return;
      }
      const domain = document.getElementById('domain').value.trim();
      if (!domain) {
        setStatus('Domain is required.', 'warn');
        return;
      }
      try {
        await api(`/api/v1/apps/${selectedApp}/domains`, {
          method:'POST',
          headers:{'content-type':'application/json'},
          body:JSON.stringify({ domain, tls_mode:'managed' })
        });
        document.getElementById('domain').value = '';
        setStatus(`Domain ${domain} added.`);
        await refreshDomains();
      } catch (error) {
        setStatus(`Add domain failed: ${error.message}`, 'err');
      }
    }

    async function deploy(appId) {
      try {
        const res = await api(`/api/v1/apps/${appId}/deployments`, {
          method:'POST',
          headers:{'content-type':'application/json'},
          body:'{}'
        });
        const accepted = await res.json();
        markPendingDeployment(appId, accepted.deployment_id, accepted.status, 'manual');
        setStatus('Deployment queued. Build status will update live.');
        if (selectedApp === appId) await refreshDeployments();
      } catch (error) {
        setStatus(`Deploy failed: ${error.message}`, 'err');
      }
    }

    async function resyncRebuild(appId) {
      try {
        const res = await api(`/api/v1/apps/${appId}/deployments`, {
          method:'POST',
          headers:{'content-type':'application/json'},
          body:JSON.stringify({ force_rebuild: true })
        });
        const accepted = await res.json();
        markPendingDeployment(appId, accepted.deployment_id, accepted.status, 'manual (rebuild)');
        setStatus('Resync and rebuild queued. Build status will update live.');
        if (selectedApp === appId) await refreshDeployments();
      } catch (error) {
        setStatus(`Resync/rebuild failed: ${error.message}`, 'err');
      }
    }

    async function rollback(appId) {
      try {
        const res = await api(`/api/v1/apps/${appId}/rollback`, { method:'POST' });
        const accepted = await res.json();
        markPendingDeployment(appId, accepted.deployment_id, accepted.status, 'rollback');
        setStatus('Rollback queued. Build status will update live.');
        if (selectedApp === appId) await refreshDeployments();
      } catch (error) {
        setStatus(`Rollback failed: ${error.message}`, 'err');
      }
    }

    async function logout() {
      if (logsStream) {
        logsStream.close();
        logsStream = null;
      }
      await api('/api/v1/auth/logout', { method:'POST' });
      window.location.reload();
    }

    window.addEventListener('beforeunload', () => {
      if (logsStream) logsStream.close();
    });

    renderRoutes();
    renderSelectedDeployment();
    applyDeploymentsCollapsed();
    document.getElementById('env-vars').innerHTML = '<li class="hint">Select an app.</li>';
    refreshApps().catch((error) => setStatus(`Initial load failed: ${error.message}`, 'err'));
    setInterval(() => refreshApps().catch(() => {}), 15000);
    setInterval(() => refreshDeployments().catch(() => {}), 4000);
    setInterval(() => refreshEnvVars().catch(() => {}), 10000);
  </script>
</body>
</html>
"#;

const LOGS_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Rustploy Logs Explorer</title>
  <style>
    @import url('https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600;700&family=Geist+Mono:wght@500&display=swap');
    :root {
      --bg: oklch(0.13 0.005 260);
      --foreground: oklch(0.95 0 0);
      --card: oklch(0.16 0.005 260);
      --sidebar: oklch(0.11 0.005 260);
      --secondary: oklch(0.2 0.005 260);
      --line: oklch(0.24 0.008 260);
      --muted: oklch(0.55 0.01 260);
      --primary: oklch(0.65 0.2 145);
      --danger: oklch(0.55 0.2 25);
      --warning: oklch(0.75 0.15 65);
      --ok: oklch(0.65 0.2 145);
      --shadow: 0 24px 60px rgba(0, 0, 0, 0.4);
    }
    * { box-sizing: border-box; }
    html {
      min-height: 100%;
      background: var(--bg);
      overscroll-behavior-y: none;
    }
    body {
      margin: 0;
      min-height: 100vh;
      color: var(--foreground);
      font-family: 'Geist', ui-sans-serif, system-ui, -apple-system, sans-serif;
      overscroll-behavior-y: none;
      background:
        radial-gradient(1200px 560px at 100% -140px, rgba(89, 97, 249, 0.12), transparent 70%),
        radial-gradient(950px 500px at -10% 0%, rgba(39, 191, 122, 0.14), transparent 60%),
        linear-gradient(var(--bg), var(--bg));
    }
    .shell {
      min-height: 100vh;
      display: grid;
      grid-template-columns: 244px minmax(0, 1fr);
    }
    .sidebar {
      background: color-mix(in oklab, var(--sidebar) 90%, black 10%);
      border-right: 1px solid var(--line);
      padding: 16px 12px;
      display: flex;
      flex-direction: column;
      gap: 12px;
      min-height: 100vh;
      position: sticky;
      top: 0;
    }
    .brand-block {
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 2px 6px 14px;
      border-bottom: 1px solid var(--line);
    }
    .brand-icon {
      width: 32px;
      height: 32px;
      border-radius: 9px;
      background: color-mix(in oklab, var(--primary) 82%, white 18%);
      color: #081810;
      display: grid;
      place-items: center;
      font-size: 16px;
      font-weight: 700;
    }
    .brand-title { margin: 0; font-size: 13px; font-weight: 600; }
    .brand-sub { margin: 1px 0 0; font-size: 11px; color: var(--muted); }
    .side-group {
      display: grid;
      gap: 4px;
      margin-top: 8px;
    }
    .side-item {
      border: 1px solid transparent;
      border-radius: 9px;
      background: transparent;
      color: var(--muted);
      font-size: 12px;
      text-align: left;
      padding: 9px 10px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      box-shadow: none;
      font-weight: 500;
      cursor: pointer;
    }
    .side-item strong { color: inherit; }
    .side-item.active {
      background: color-mix(in oklab, var(--primary) 14%, transparent);
      border-color: color-mix(in oklab, var(--primary) 24%, var(--line));
      color: color-mix(in oklab, var(--primary) 80%, white 20%);
      font-weight: 600;
    }
    .side-count {
      font-size: 10px;
      border-radius: 999px;
      background: color-mix(in oklab, var(--secondary) 88%, black 12%);
      border: 1px solid var(--line);
      padding: 2px 7px;
      color: var(--muted);
      font-family: 'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
    }
    .sidebar-bottom {
      margin-top: auto;
      padding-top: 10px;
      border-top: 1px solid var(--line);
    }
    .sidebar-bottom button {
      width: 100%;
    }
    .main {
      padding: 18px;
      min-width: 0;
      display: grid;
      gap: 12px;
      align-content: start;
    }
    .topbar {
      display: flex;
      gap: 12px;
      align-items: center;
      justify-content: space-between;
      border: 1px solid var(--line);
      border-radius: 12px;
      background: color-mix(in oklab, var(--card) 88%, black 12%);
      box-shadow: var(--shadow);
      padding: 12px 14px;
    }
    .title h1 {
      margin: 0;
      font-size: 18px;
      line-height: 1.2;
      letter-spacing: -0.01em;
    }
    .title p { margin: 2px 0 0; font-size: 12px; color: var(--muted); }
    .actions {
      display: flex;
      align-items: center;
      gap: 8px;
      flex-wrap: wrap;
    }
    .status {
      min-height: 36px;
      border-radius: 10px;
      border: 1px solid color-mix(in oklab, var(--primary) 30%, var(--line));
      background: color-mix(in oklab, var(--primary) 12%, transparent);
      color: color-mix(in oklab, var(--primary) 78%, white 22%);
      padding: 8px 11px;
      font-size: 13px;
      display: flex;
      align-items: center;
    }
    .status.warn {
      border-color: color-mix(in oklab, var(--warning) 35%, var(--line));
      background: color-mix(in oklab, var(--warning) 14%, transparent);
      color: color-mix(in oklab, var(--warning) 78%, white 22%);
    }
    .status.err {
      border-color: color-mix(in oklab, var(--danger) 35%, var(--line));
      background: color-mix(in oklab, var(--danger) 14%, transparent);
      color: color-mix(in oklab, var(--danger) 80%, white 20%);
    }
    .layout {
      display: grid;
      gap: 12px;
      grid-template-columns: minmax(0, 1.4fr) minmax(0, 1fr);
      align-items: start;
      min-width: 0;
    }
    .stack {
      display: grid;
      gap: 12px;
      align-content: start;
      min-width: 0;
    }
    .card {
      border-radius: 11px;
      border: 1px solid var(--line);
      background: color-mix(in oklab, var(--card) 88%, black 12%);
      box-shadow: 0 12px 22px -18px rgba(0, 0, 0, 0.8);
      min-width: 0;
    }
    .card-head {
      padding: 12px 14px;
      border-bottom: 1px solid var(--line);
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 10px;
    }
    .card-head h3 {
      margin: 0;
      font-size: 13px;
      font-weight: 600;
    }
    .card-body {
      padding: 12px 14px;
      min-width: 0;
    }
    .grid {
      display: grid;
      gap: 8px;
      grid-template-columns: 1fr 1fr;
    }
    .grid.three {
      grid-template-columns: 1fr 1fr 1fr;
    }
    .field {
      display: grid;
      gap: 4px;
    }
    .field label {
      font-size: 11px;
      color: var(--muted);
    }
    input, select {
      width: 100%;
      padding: 9px 10px;
      border: 1px solid var(--line);
      border-radius: 9px;
      background: color-mix(in oklab, var(--secondary) 90%, black 10%);
      color: var(--foreground);
      font: inherit;
      font-size: 13px;
      transition: border-color .12s ease, box-shadow .12s ease;
    }
    input::placeholder { color: color-mix(in oklab, var(--muted) 84%, white 16%); }
    input:focus, select:focus {
      outline: none;
      border-color: color-mix(in oklab, var(--primary) 50%, var(--line));
      box-shadow: 0 0 0 3px color-mix(in oklab, var(--primary) 16%, transparent);
    }
    button {
      border: 0;
      border-radius: 9px;
      padding: 9px 11px;
      font: inherit;
      font-size: 12px;
      font-weight: 600;
      color: #07170f;
      background: color-mix(in oklab, var(--primary) 88%, white 12%);
      cursor: pointer;
      white-space: nowrap;
      transition: filter .16s ease, transform .08s ease;
      box-shadow: none;
    }
    button:hover { filter: brightness(1.04); }
    button:active { transform: translateY(1px); }
    button.secondary {
      background: color-mix(in oklab, var(--secondary) 88%, black 12%);
      color: color-mix(in oklab, var(--muted) 75%, white 25%);
      border: 1px solid var(--line);
      font-weight: 500;
    }
    .list {
      list-style: none;
      margin: 0;
      padding: 0;
      display: grid;
      gap: 7px;
      max-height: 410px;
      overflow: auto;
    }
    .item-row {
      border: 1px solid var(--line);
      border-radius: 10px;
      background: color-mix(in oklab, var(--secondary) 78%, black 22%);
      padding: 9px 10px;
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 10px;
      align-items: start;
      min-width: 0;
    }
    .item-row.selected {
      border-color: color-mix(in oklab, var(--primary) 42%, var(--line));
      background: color-mix(in oklab, var(--primary) 12%, transparent);
    }
    .item-main {
      display: grid;
      gap: 2px;
      min-width: 0;
      width: 100%;
    }
    .item-main strong {
      font-size: 12px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .item-main code {
      display: block;
      font-family: 'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
      font-size: 11px;
      color: var(--muted);
      white-space: normal;
      overflow-wrap: break-word;
    }
    .item-actions {
      display: flex;
      gap: 6px;
      flex-wrap: wrap;
      justify-content: flex-end;
      align-items: flex-start;
    }
    .item-actions button {
      padding: 6px 8px;
      font-size: 11px;
    }
    .pill-status {
      border-radius: 999px;
      font-size: 10px;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: .04em;
      padding: 4px 8px;
      border: 1px solid color-mix(in oklab, var(--ok) 36%, var(--line));
      background: color-mix(in oklab, var(--ok) 12%, transparent);
      color: color-mix(in oklab, var(--ok) 80%, white 20%);
    }
    .pill-status.status-queued,
    .pill-status.status-deploying,
    .pill-status.status-retrying {
      border-color: color-mix(in oklab, var(--warning) 40%, var(--line));
      background: color-mix(in oklab, var(--warning) 14%, transparent);
      color: color-mix(in oklab, var(--warning) 80%, white 20%);
    }
    .pill-status.status-healthy {
      border-color: color-mix(in oklab, var(--ok) 36%, var(--line));
      background: color-mix(in oklab, var(--ok) 12%, transparent);
      color: color-mix(in oklab, var(--ok) 80%, white 20%);
    }
    .pill-status.status-failed {
      border-color: color-mix(in oklab, var(--danger) 42%, var(--line));
      background: color-mix(in oklab, var(--danger) 15%, transparent);
      color: color-mix(in oklab, var(--danger) 82%, white 18%);
    }
    pre {
      margin: 0;
      min-height: 420px;
      max-height: 620px;
      overflow: auto;
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      border-radius: 10px;
      background: color-mix(in oklab, var(--sidebar) 70%, black 30%);
      color: color-mix(in oklab, var(--foreground) 92%, white 8%);
      font-family: 'Geist Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
      font-size: 12px;
      line-height: 1.45;
      border: 1px solid var(--line);
      padding: 11px;
    }
    .hint {
      color: var(--muted);
      font-size: 12px;
      margin-top: 8px;
    }
    .checks {
      display: flex;
      gap: 10px;
      flex-wrap: wrap;
      align-items: center;
      color: var(--muted);
      font-size: 11px;
    }
    .checks label {
      display: inline-flex;
      align-items: center;
      gap: 5px;
    }
    @media (max-width: 1120px) {
      .layout { grid-template-columns: 1fr; }
      .grid, .grid.three { grid-template-columns: 1fr; }
    }
    @media (max-width: 920px) {
      .shell { grid-template-columns: 1fr; }
      .sidebar {
        position: static;
        min-height: auto;
      }
    }
    @media (max-width: 640px) {
      .main { padding: 12px; }
      .topbar {
        flex-direction: column;
        align-items: stretch;
      }
      .actions { justify-content: space-between; }
    }
  </style>
</head>
<body>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand-block">
        <div class="brand-icon">R</div>
        <div>
          <p class="brand-title">Rustploy</p>
          <p class="brand-sub">Control Plane</p>
        </div>
      </div>

      <div class="side-group">
        <button class="side-item" onclick="window.location.href='/'"><strong>Dashboard</strong> <span class="side-count">01</span></button>
        <button class="side-item active"><strong>Logs Explorer</strong></button>
      </div>

      <div class="sidebar-bottom">
        <button class="secondary" onclick="window.location.href='/'">Back To Dashboard</button>
      </div>
    </aside>

    <main class="main">
      <header class="topbar">
        <div class="title">
          <h1>Logs Explorer</h1>
          <p>Query deployments, filter results, and inspect logs across apps.</p>
        </div>
        <div class="actions">
          <button class="secondary" onclick="window.location.href='/'">Dashboard</button>
          <button class="secondary" onclick="logout()">Logout</button>
        </div>
      </header>

      <div id="status" class="status">Ready.</div>

      <section class="layout">
        <div class="stack">
          <article class="card">
            <div class="card-head">
              <h3>Query Filters</h3>
            </div>
            <div class="card-body">
              <div class="grid three">
                <div class="field">
                  <label for="filter-app">App</label>
                  <select id="filter-app"></select>
                </div>
                <div class="field">
                  <label for="filter-status">Status</label>
                  <select id="filter-status">
                    <option value="all">All</option>
                    <option value="healthy">healthy</option>
                    <option value="failed">failed</option>
                    <option value="queued">queued</option>
                    <option value="deploying">deploying</option>
                    <option value="retrying">retrying</option>
                  </select>
                </div>
                <div class="field">
                  <label for="filter-hours">Updated Within (hours)</label>
                  <input id="filter-hours" type="number" min="1" value="168" />
                </div>
              </div>
              <div class="grid" style="margin-top:8px">
                <div class="field">
                  <label for="filter-source">Source Ref Contains</label>
                  <input id="filter-source" placeholder="main / feature branch / manual" />
                </div>
                <div class="field">
                  <label for="filter-deployment">Deployment Query</label>
                  <input id="filter-deployment" placeholder="id, commit, image, app name" />
                </div>
              </div>
              <div class="item-actions" style="margin-top:10px">
                <button id="run-query-btn" onclick="runQuery()">Run Query</button>
                <button class="secondary" onclick="clearFilters()">Clear Filters</button>
              </div>
              <p id="results-meta" class="hint">No query run yet.</p>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Deployment Results</h3>
            </div>
            <div class="card-body">
              <ul id="deployments-list" class="list"></ul>
            </div>
          </article>
        </div>

        <div class="stack">
          <article class="card">
            <div class="card-head">
              <h3>Selected Deployment</h3>
            </div>
            <div class="card-body">
              <ul id="selected-deployment" class="list"></ul>
            </div>
          </article>

          <article class="card">
            <div class="card-head">
              <h3>Logs</h3>
            </div>
            <div class="card-body">
              <div class="grid">
                <div class="field">
                  <label for="log-query">Log Text Filter</label>
                  <input id="log-query" placeholder="error, warning, stack trace..." />
                </div>
                <div class="item-actions" style="align-items:end">
                  <button class="secondary" onclick="applyLogFilter()">Apply</button>
                  <button class="secondary" onclick="clearLogFilter()">Clear</button>
                </div>
              </div>
              <div class="checks" style="margin-top:8px">
                <label><input id="log-case-sensitive" type="checkbox" /> Case sensitive</label>
                <label><input id="log-only-matches" type="checkbox" checked /> Show matching lines only</label>
              </div>
              <p id="logs-meta" class="hint">No deployment selected.</p>
              <pre id="logs-output">(no logs loaded)</pre>
            </div>
          </article>
        </div>
      </section>
    </main>
  </div>
  <script>
    let apps = [];
    let allDeployments = [];
    let filteredDeployments = [];
    let selectedDeployment = null;
    let rawLogs = '';
    let statusTimeout = null;

    const ACTIVE_DEPLOYMENT_STATUSES = new Set(['queued', 'deploying', 'retrying']);

    function normalizeDeploymentStatus(value) {
      return (value || 'queued').toString().toLowerCase();
    }

    function deploymentPillClass(value) {
      const status = normalizeDeploymentStatus(value);
      if (status === 'healthy') return 'status-healthy';
      if (status === 'failed') return 'status-failed';
      if (status === 'deploying') return 'status-deploying';
      if (status === 'retrying') return 'status-retrying';
      return 'status-queued';
    }

    function isActiveDeploymentStatus(value) {
      return ACTIVE_DEPLOYMENT_STATUSES.has(normalizeDeploymentStatus(value));
    }

    function setStatus(message, level = 'ok') {
      const el = document.getElementById('status');
      el.textContent = message;
      el.className = 'status';
      if (level === 'warn') el.classList.add('warn');
      if (level === 'err') el.classList.add('err');
      if (statusTimeout) clearTimeout(statusTimeout);
      statusTimeout = setTimeout(() => {
        el.textContent = 'Ready.';
        el.className = 'status';
      }, 4500);
    }

    async function api(path, opts = {}) {
      const res = await fetch(path, { credentials: 'include', ...opts });
      if (res.status === 401) {
        window.location.reload();
        throw new Error('unauthorized');
      }
      if (!res.ok) {
        const text = await res.text();
        throw new Error(text || `request failed: ${res.status}`);
      }
      return res;
    }

    function formatUnixMs(value) {
      if (typeof value !== 'number' || Number.isNaN(value) || value <= 0) {
        return 'unknown';
      }
      try {
        return new Date(value).toLocaleString();
      } catch (_) {
        return 'unknown';
      }
    }

    function withinHours(updatedAtUnixMs, hours) {
      if (!Number.isFinite(hours) || hours <= 0) return true;
      if (typeof updatedAtUnixMs !== 'number' || updatedAtUnixMs <= 0) return false;
      return Date.now() - updatedAtUnixMs <= hours * 60 * 60 * 1000;
    }

    function renderAppOptions() {
      const select = document.getElementById('filter-app');
      const selected = select.value || 'all';
      select.innerHTML = '';
      const allOption = document.createElement('option');
      allOption.value = 'all';
      allOption.textContent = 'All apps';
      select.appendChild(allOption);
      for (const app of apps) {
        const option = document.createElement('option');
        option.value = app.id;
        option.textContent = app.name;
        select.appendChild(option);
      }
      select.value = apps.some((app) => app.id === selected) ? selected : 'all';
    }

    async function loadApps() {
      const res = await api('/api/v1/apps');
      const data = await res.json();
      apps = data.items || [];
      renderAppOptions();
    }

    function applyFiltersAndRender() {
      const appId = document.getElementById('filter-app').value;
      const status = document.getElementById('filter-status').value;
      const sourceQuery = document.getElementById('filter-source').value.trim().toLowerCase();
      const deploymentQuery = document.getElementById('filter-deployment').value.trim().toLowerCase();
      const hours = Number.parseFloat(document.getElementById('filter-hours').value);

      filteredDeployments = allDeployments.filter((deployment) => {
        const deploymentStatus = normalizeDeploymentStatus(deployment.status);
        if (appId !== 'all' && deployment.app_id !== appId) return false;
        if (status !== 'all' && deploymentStatus !== status) return false;
        if (sourceQuery && !String(deployment.source_ref || '').toLowerCase().includes(sourceQuery)) return false;
        if (!withinHours(deployment.updated_at_unix_ms, hours)) return false;
        if (deploymentQuery) {
          const queryHaystack = [
            deployment.id,
            deployment.app_name,
            deployment.source_ref || '',
            deployment.commit_sha || '',
            deployment.image_ref || ''
          ]
            .join(' ')
            .toLowerCase();
          if (!queryHaystack.includes(deploymentQuery)) return false;
        }
        return true;
      });

      renderDeployments();
      document.getElementById('results-meta').textContent =
        `${filteredDeployments.length} result(s) from ${allDeployments.length} deployment(s).`;
    }

    async function runQuery() {
      if (apps.length === 0) {
        await loadApps();
      }
      setStatus('Querying deployment data...');

      const appId = document.getElementById('filter-app').value || 'all';
      const targets = appId === 'all' ? apps : apps.filter((app) => app.id === appId);
      if (targets.length === 0) {
        allDeployments = [];
        filteredDeployments = [];
        selectedDeployment = null;
        rawLogs = '';
        renderDeployments();
        renderSelectedDeployment();
        applyLogFilter();
        setStatus('No apps available for querying.', 'warn');
        return;
      }

      const settled = await Promise.allSettled(
        targets.map(async (app) => {
          const res = await api(`/api/v1/apps/${app.id}/deployments`);
          const data = await res.json();
          return (data.items || []).map((deployment) => ({
            ...deployment,
            app_id: app.id,
            app_name: app.name
          }));
        })
      );

      const successful = [];
      const failedApps = [];
      settled.forEach((result, index) => {
        if (result.status === 'fulfilled') {
          successful.push(...result.value);
          return;
        }
        const app = targets[index];
        failedApps.push(app);
        console.error('deployment query failed', app ? app.id : 'unknown-app', result.reason);
      });

      allDeployments = successful
        .sort((a, b) => (b.updated_at_unix_ms || 0) - (a.updated_at_unix_ms || 0));

      if (!selectedDeployment || !allDeployments.some((item) => item.id === selectedDeployment.id)) {
        selectedDeployment = allDeployments[0] || null;
      } else {
        selectedDeployment =
          allDeployments.find((item) => item.id === selectedDeployment.id) || selectedDeployment;
      }

      applyFiltersAndRender();
      renderSelectedDeployment();
      if (failedApps.length > 0) {
        const failedList = failedApps.map((app) => app.name || app.id).join(', ');
        setStatus(
          `Query complete with partial failures (${failedList}). Loaded ${allDeployments.length} deployment(s).`,
          'warn'
        );
      } else {
        setStatus(`Query complete. Loaded ${allDeployments.length} deployment(s).`);
      }
    }

    function renderDeployments() {
      const el = document.getElementById('deployments-list');
      el.innerHTML = '';

      if (filteredDeployments.length === 0) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'No deployment results for the current filters.';
        el.appendChild(empty);
        return;
      }

      for (const deployment of filteredDeployments) {
        const status = normalizeDeploymentStatus(deployment.status);
        const isSelected = selectedDeployment && selectedDeployment.id === deployment.id;
        const item = document.createElement('li');
        item.className = `item-row ${isSelected ? 'selected' : ''} ${
          isActiveDeploymentStatus(status) ? 'in-progress' : ''
        }`.trim();

        const main = document.createElement('div');
        main.className = 'item-main';
        const strong = document.createElement('strong');
        strong.textContent = deployment.app_name;
        const codeStatus = document.createElement('code');
        const statusPill = document.createElement('span');
        statusPill.className = `pill-status ${deploymentPillClass(status)}`;
        statusPill.textContent = status;
        codeStatus.appendChild(statusPill);
        codeStatus.appendChild(document.createTextNode(` ${deployment.id}`));
        const codeMeta = document.createElement('code');
        codeMeta.textContent = `source=${deployment.source_ref || 'manual'} | updated=${formatUnixMs(
          deployment.updated_at_unix_ms
        )}`;
        main.appendChild(strong);
        main.appendChild(codeStatus);
        main.appendChild(codeMeta);

        const actions = document.createElement('div');
        actions.className = 'item-actions';
        const selectButton = document.createElement('button');
        selectButton.className = 'secondary';
        selectButton.textContent = isSelected ? 'Selected' : 'Select';
        selectButton.onclick = () => selectDeployment(deployment.app_id, deployment.id);
        const logsButton = document.createElement('button');
        logsButton.className = 'secondary';
        logsButton.textContent = 'Logs';
        logsButton.onclick = () => openDeploymentLogs(deployment.app_id, deployment.id);
        actions.appendChild(selectButton);
        actions.appendChild(logsButton);

        item.appendChild(main);
        item.appendChild(actions);
        el.appendChild(item);
      }
    }

    function renderSelectedDeployment() {
      const el = document.getElementById('selected-deployment');
      el.innerHTML = '';

      if (!selectedDeployment) {
        const empty = document.createElement('li');
        empty.className = 'hint';
        empty.textContent = 'Select a deployment to view condensed details.';
        el.appendChild(empty);
        return;
      }

      const status = normalizeDeploymentStatus(selectedDeployment.status);
      const rowA = document.createElement('li');
      rowA.className = `item-row ${isActiveDeploymentStatus(status) ? 'in-progress' : ''}`.trim();

      const rowAMain = document.createElement('div');
      rowAMain.className = 'item-main';
      const rowAStrong = document.createElement('strong');
      rowAStrong.textContent = selectedDeployment.app_name;
      const rowAStatus = document.createElement('code');
      const rowAPill = document.createElement('span');
      rowAPill.className = `pill-status ${deploymentPillClass(status)}`;
      rowAPill.textContent = status;
      rowAStatus.appendChild(rowAPill);
      rowAStatus.appendChild(document.createTextNode(` ${selectedDeployment.id}`));
      const rowASource = document.createElement('code');
      rowASource.textContent = `source=${selectedDeployment.source_ref || 'manual'}`;
      rowAMain.appendChild(rowAStrong);
      rowAMain.appendChild(rowAStatus);
      rowAMain.appendChild(rowASource);
      rowA.appendChild(rowAMain);
      el.appendChild(rowA);

      const rowB = document.createElement('li');
      rowB.className = 'item-row';
      const rowBMain = document.createElement('div');
      rowBMain.className = 'item-main';
      const rowBStrong = document.createElement('strong');
      rowBStrong.textContent = 'Metadata';
      const rowBCommit = document.createElement('code');
      rowBCommit.textContent = `commit=${selectedDeployment.commit_sha || 'n/a'}`;
      const rowBImage = document.createElement('code');
      rowBImage.textContent = `image=${selectedDeployment.image_ref || 'n/a'}`;
      const rowBUpdated = document.createElement('code');
      rowBUpdated.textContent = `updated=${formatUnixMs(selectedDeployment.updated_at_unix_ms)}`;
      rowBMain.appendChild(rowBStrong);
      rowBMain.appendChild(rowBCommit);
      rowBMain.appendChild(rowBImage);
      rowBMain.appendChild(rowBUpdated);
      rowB.appendChild(rowBMain);
      el.appendChild(rowB);
    }

    function selectDeployment(appId, deploymentId) {
      const found = allDeployments.find(
        (deployment) => deployment.app_id === appId && deployment.id === deploymentId
      );
      if (!found) return;
      selectedDeployment = found;
      renderSelectedDeployment();
      renderDeployments();
    }

    async function openDeploymentLogs(appId, deploymentId) {
      selectDeployment(appId, deploymentId);
      try {
        setStatus('Fetching deployment logs...');
        const res = await api(`/api/v1/apps/${appId}/deployments/${deploymentId}/logs`);
        const data = await res.json();
        rawLogs = data.logs || '';
        applyLogFilter();
        setStatus('Logs loaded.');
      } catch (error) {
        setStatus(`Unable to load logs: ${error.message}`, 'err');
      }
    }

    function applyLogFilter() {
      const query = document.getElementById('log-query').value;
      const caseSensitive = document.getElementById('log-case-sensitive').checked;
      const onlyMatches = document.getElementById('log-only-matches').checked;
      const output = document.getElementById('logs-output');
      const meta = document.getElementById('logs-meta');

      if (!rawLogs) {
        output.textContent = '(no logs loaded)';
        meta.textContent = 'No logs loaded.';
        return;
      }

      const lines = rawLogs.split('\n');
      if (!query) {
        output.textContent = rawLogs;
        meta.textContent = `${lines.length} line(s) loaded.`;
        return;
      }

      const target = caseSensitive ? query : query.toLowerCase();
      const matches = lines.filter((line) => {
        const haystack = caseSensitive ? line : line.toLowerCase();
        return haystack.includes(target);
      });

      if (onlyMatches) {
        output.textContent = matches.length > 0 ? matches.join('\n') : '(no matching lines)';
      } else {
        output.textContent = rawLogs;
      }

      meta.textContent = `${matches.length} matching line(s) out of ${lines.length}.`;
    }

    function clearLogFilter() {
      document.getElementById('log-query').value = '';
      document.getElementById('log-case-sensitive').checked = false;
      document.getElementById('log-only-matches').checked = true;
      applyLogFilter();
    }

    function clearFilters() {
      document.getElementById('filter-app').value = 'all';
      document.getElementById('filter-status').value = 'all';
      document.getElementById('filter-hours').value = '168';
      document.getElementById('filter-source').value = '';
      document.getElementById('filter-deployment').value = '';
      applyFiltersAndRender();
    }

    async function logout() {
      await api('/api/v1/auth/logout', { method: 'POST' });
      window.location.reload();
    }

    document.getElementById('filter-source').addEventListener('input', applyFiltersAndRender);
    document.getElementById('filter-deployment').addEventListener('input', applyFiltersAndRender);
    document.getElementById('filter-status').addEventListener('change', applyFiltersAndRender);
    document.getElementById('filter-app').addEventListener('change', applyFiltersAndRender);
    document.getElementById('filter-hours').addEventListener('change', applyFiltersAndRender);

    document.getElementById('log-query').addEventListener('input', applyLogFilter);
    document.getElementById('log-case-sensitive').addEventListener('change', applyLogFilter);
    document.getElementById('log-only-matches').addEventListener('change', applyLogFilter);

    document.getElementById('deployments-list').innerHTML = '<li class="hint">Run a query to load deployment data.</li>';
    document.getElementById('selected-deployment').innerHTML = '<li class="hint">Select a deployment to view condensed details.</li>';
    runQuery().catch((error) => setStatus(`Initial load failed: ${error.message}`, 'err'));
  </script>
</body>
</html>
"#;

async fn auth_login(
    State(state): State<AppState>,
    Json(payload): Json<AuthLoginRequest>,
) -> Result<impl axum::response::IntoResponse, ApiJsonError> {
    state
        .check_rate_limit("auth:login", state.api_rate_limit_per_minute)
        .map_err(status_to_api_error)?;

    let email = payload.email.trim().to_ascii_lowercase();
    if email.is_empty() || payload.password.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "email and password are required",
            Vec::new(),
        ));
    }

    let Some((user_id, password_hash, role)) = state
        .db
        .find_user_by_email(&email)
        .map_err(|error| status_to_api_error(internal_error(error)))?
    else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid credentials",
            Vec::new(),
        ));
    };

    if token_hash(&payload.password) != password_hash {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid credentials",
            Vec::new(),
        ));
    }

    let session_token = format!(
        "sess_{}.{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    let now = now_unix_ms();
    let expires_at = now.saturating_add(state.session_ttl_ms);
    state
        .db
        .create_session(user_id, &token_hash(&session_token), expires_at, now)
        .map_err(|error| status_to_api_error(internal_error(error)))?;

    let cookie = format!(
        "{SESSION_COOKIE_NAME}={session_token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        state.session_ttl_ms / 1000
    );

    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(AuthSessionResponse {
            user_id,
            email,
            role,
            session_expires_at_unix_ms: expires_at,
        }),
    ))
}

async fn auth_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl axum::response::IntoResponse, ApiJsonError> {
    if let Some(session_token) = extract_cookie_value(&headers, SESSION_COOKIE_NAME) {
        let _ = state
            .db
            .revoke_session(&token_hash(&session_token), now_unix_ms());
    }
    let cookie =
        format!("{SESSION_COOKIE_NAME}=deleted; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]))
}

async fn auth_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AuthSessionResponse>, ApiJsonError> {
    let session = authorize_session_request(&state, &headers)
        .map_err(status_to_api_error)?
        .ok_or_else(|| {
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "active session required",
                Vec::new(),
            )
        })?;
    let now = now_unix_ms();
    Ok(Json(AuthSessionResponse {
        user_id: session.user_id,
        email: session.email,
        role: session.role,
        session_expires_at_unix_ms: now.saturating_add(state.session_ttl_ms),
    }))
}

async fn auth_password_reset_request(
    State(state): State<AppState>,
    Json(payload): Json<PasswordResetRequest>,
) -> Result<(StatusCode, Json<PasswordResetRequestedResponse>), ApiJsonError> {
    state
        .check_rate_limit(
            "auth:password_reset_request",
            state.api_rate_limit_per_minute,
        )
        .map_err(status_to_api_error)?;

    let email = payload.email.trim().to_ascii_lowercase();
    if email.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "email is required",
            Vec::new(),
        ));
    }

    let reset_token = format!(
        "reset_{}.{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    let now = now_unix_ms();
    let expires_at = now.saturating_add(state.password_reset_ttl_ms);
    let issued = state
        .db
        .issue_password_reset(&email, &token_hash(&reset_token), expires_at, now)
        .map_err(|error| status_to_api_error(internal_error(error)))?
        .is_some();

    if !issued {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "user not found",
            Vec::new(),
        ));
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(PasswordResetRequestedResponse {
            accepted: true,
            reset_token,
            expires_at_unix_ms: expires_at,
        }),
    ))
}

async fn auth_password_reset_confirm(
    State(state): State<AppState>,
    Json(payload): Json<PasswordResetConfirmRequest>,
) -> Result<(StatusCode, Json<PasswordResetConfirmedResponse>), ApiJsonError> {
    state
        .check_rate_limit(
            "auth:password_reset_confirm",
            state.api_rate_limit_per_minute,
        )
        .map_err(status_to_api_error)?;

    if payload.reset_token.trim().is_empty() || payload.new_password.len() < 8 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "reset_token is required and new_password must be at least 8 chars",
            Vec::new(),
        ));
    }

    let updated = state
        .db
        .confirm_password_reset(
            &token_hash(payload.reset_token.trim()),
            &token_hash(&payload.new_password),
            now_unix_ms(),
        )
        .map_err(|error| status_to_api_error(internal_error(error)))?;

    if !updated {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid or expired reset token",
            Vec::new(),
        ));
    }

    Ok((
        StatusCode::OK,
        Json(PasswordResetConfirmedResponse { accepted: true }),
    ))
}

async fn import_app(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ImportAppRequest>,
) -> Result<(StatusCode, Json<ImportAppResponse>), ApiJsonError> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "POST",
        "/api/v1/apps/import",
    )
    .map_err(status_to_api_error)?;

    let owner = payload.repository.owner.trim().to_string();
    let repo = payload.repository.name.trim().to_string();
    if owner.is_empty() || repo.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "repository owner and name are required",
            vec![ApiErrorDetail {
                field: "repository".to_string(),
                message: "owner and name must be non-empty".to_string(),
            }],
        ));
    }

    let branch = payload
        .source
        .as_ref()
        .and_then(|source| source.branch.clone())
        .or_else(|| payload.repository.default_branch.clone())
        .unwrap_or_else(|| "main".to_string());

    let clone_url = payload
        .repository
        .clone_url
        .clone()
        .unwrap_or_else(|| format!("https://github.com/{owner}/{repo}.git"));
    let temp_path = std::env::temp_dir().join(format!("rustploy-import-{}", Uuid::new_v4()));
    clone_repository(&clone_url, &branch, &temp_path).map_err(status_to_api_error)?;
    let import_result = (|| -> Result<ImportAppResponse, ApiJsonError> {
        let manifest = read_manifest(&temp_path).map_err(|errors| {
            api_error(
                StatusCode::BAD_REQUEST,
                "invalid_manifest",
                "rustploy.yaml failed validation",
                errors
                    .into_iter()
                    .map(|error| ApiErrorDetail {
                        field: error.field,
                        message: error.message,
                    })
                    .collect(),
            )
        })?;
        let manifest_json = manifest
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "failed to serialize parsed manifest",
                    Vec::new(),
                )
            })?;
        let compose = read_compose_summary(&temp_path).map_err(|error| {
            api_error(
                StatusCode::BAD_REQUEST,
                "invalid_compose",
                "failed to parse docker compose file",
                vec![ApiErrorDetail {
                    field: "docker-compose.yml".to_string(),
                    message: error.to_string(),
                }],
            )
        })?;
        let detection = detect_build_profile(&temp_path, compose.as_ref()).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "build_detection_failed",
                "failed to detect project build profile (package.json, Dockerfile, or docker-compose required)",
                vec![ApiErrorDetail {
                    field: "project".to_string(),
                    message:
                        "missing or invalid package.json; expected Dockerfile or docker-compose as fallback"
                            .to_string(),
                }],
            )
        })?;
        let build_mode = payload
            .build_mode
            .clone()
            .or_else(|| {
                manifest
                    .as_ref()
                    .and_then(|value| value.build.as_ref()?.mode.clone())
            })
            .unwrap_or_else(|| {
                if compose.is_some() {
                    "compose".to_string()
                } else {
                    "auto".to_string()
                }
            });
        if build_mode != "auto" && build_mode != "dockerfile" && build_mode != "compose" {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "build_mode must be auto, dockerfile, or compose",
                vec![ApiErrorDetail {
                    field: "build_mode".to_string(),
                    message: "supported values are auto, dockerfile, and compose".to_string(),
                }],
            ));
        }

        let app_name = manifest
            .as_ref()
            .and_then(|value| value.app.as_ref()?.name.clone())
            .unwrap_or_else(|| repo.clone());

        let now = now_unix_ms();
        let app = match state
            .db
            .create_app(&app_name, now)
            .map_err(|error| status_to_api_error(internal_error(error)))?
        {
            Some(created) => created,
            None => state
                .db
                .find_app_by_name(&app_name)
                .map_err(|error| status_to_api_error(internal_error(error)))?
                .ok_or_else(|| {
                    api_error(
                        StatusCode::CONFLICT,
                        "conflict",
                        "app name already exists",
                        vec![ApiErrorDetail {
                            field: "app.name".to_string(),
                            message: app_name.clone(),
                        }],
                    )
                })?,
        };

        let dependency_profile = merge_dependency_profile(
            payload.dependency_profile.clone(),
            manifest.as_ref(),
            compose.as_ref(),
        );

        state
            .db
            .upsert_github_integration(
                app.id,
                &GithubConnectRequest {
                    owner: owner.clone(),
                    repo: repo.clone(),
                    branch: branch.clone(),
                    installation_id: None,
                },
                now,
            )
            .map_err(|error| status_to_api_error(internal_error(error)))?;
        state
            .db
            .upsert_import_config(
                &ImportConfigRecord {
                    app_id: app.id,
                    repository: RepositoryRef {
                        provider: payload.repository.provider.clone(),
                        owner: owner.clone(),
                        name: repo.clone(),
                        clone_url: Some(clone_url.clone()),
                        default_branch: Some(branch.clone()),
                    },
                    source: SourceRef {
                        branch: Some(branch.clone()),
                        commit_sha: payload
                            .source
                            .as_ref()
                            .and_then(|source| source.commit_sha.clone()),
                    },
                    build_mode,
                    detection: detection.clone(),
                    compose: compose.clone(),
                    dependency_profile,
                    manifest_json,
                },
                now,
            )
            .map_err(|error| status_to_api_error(internal_error(error)))?;

        Ok(ImportAppResponse {
            app: app.clone(),
            detection,
            compose,
            next_action: NextAction {
                action_type: "create_deployment".to_string(),
                deploy_endpoint: format!("/api/v1/apps/{}/deployments", app.id),
            },
        })
    })();
    let _ = std::fs::remove_dir_all(&temp_path);

    import_result.map(|imported| (StatusCode::CREATED, Json(imported)))
}

async fn create_app(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateAppRequest>,
) -> Result<(StatusCode, Json<AppSummary>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "POST",
        "/api/v1/apps",
    )?;

    let name = payload.name.trim();
    if name.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    match state
        .db
        .create_app(name, now_unix_ms())
        .map_err(internal_error)?
    {
        Some(app) => Ok((StatusCode::CREATED, Json(app))),
        None => Err(StatusCode::CONFLICT),
    }
}

async fn list_apps(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AppListResponse>, StatusCode> {
    authorize_api_request(&state, &headers, RequiredScope::Read, "GET", "/api/v1/apps")?;

    let items = state.db.list_apps().map_err(internal_error)?;
    Ok(Json(AppListResponse { items }))
}

async fn connect_github_repo(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<GithubConnectRequest>,
) -> Result<(StatusCode, Json<GithubIntegrationSummary>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "POST",
        "/api/v1/apps/:app_id/github",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let owner = payload.owner.trim().to_string();
    let repo = payload.repo.trim().to_string();
    let branch = payload.branch.trim().to_string();
    if owner.is_empty() || repo.is_empty() || branch.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let summary = state
        .db
        .upsert_github_integration(
            app_id,
            &GithubConnectRequest {
                owner,
                repo,
                branch,
                installation_id: payload.installation_id,
            },
            now_unix_ms(),
        )
        .map_err(internal_error)?;

    Ok((StatusCode::OK, Json(summary)))
}

async fn handle_github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<GithubWebhookAccepted>), StatusCode> {
    state.check_rate_limit("webhook:github", state.webhook_rate_limit_per_minute)?;

    if headers
        .get(GITHUB_EVENT_HEADER)
        .and_then(|value| value.to_str().ok())
        != Some("push")
    {
        return Ok((
            StatusCode::ACCEPTED,
            Json(GithubWebhookAccepted {
                accepted: true,
                matched_integrations: 0,
                queued_deployments: 0,
            }),
        ));
    }

    verify_github_signature(&headers, &body, state.github_webhook_secret.as_deref())?;

    let payload: GithubPushEvent =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let branch = payload
        .git_ref
        .strip_prefix("refs/heads/")
        .ok_or(StatusCode::BAD_REQUEST)?;
    let integrations = state
        .db
        .list_github_integrations_for_push(
            &payload.repository.owner.login,
            &payload.repository.name,
            branch,
        )
        .map_err(internal_error)?;

    let mut queued = 0u32;
    for integration in &integrations {
        let request = CreateDeploymentRequest {
            source_ref: Some(format!("github:{}", payload.after)),
            image_ref: None,
            commit_sha: Some(payload.after.clone()),
            simulate_failures: Some(0),
            force_rebuild: Some(false),
        };
        state
            .db
            .create_deployment_and_enqueue_job(
                integration.app_id,
                &request,
                now_unix_ms(),
                state.job_max_attempts,
            )
            .map_err(internal_error)?;
        queued = queued.saturating_add(1);
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(GithubWebhookAccepted {
            accepted: true,
            matched_integrations: integrations.len() as u32,
            queued_deployments: queued,
        }),
    ))
}

async fn list_app_deployments(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DeploymentListResponse>, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Read,
        "GET",
        "/api/v1/apps/:app_id/deployments",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let items = state.db.list_deployments(app_id).map_err(internal_error)?;
    Ok(Json(DeploymentListResponse { items }))
}

async fn get_app_effective_config(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<EffectiveAppConfigResponse>, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Read,
        "GET",
        "/api/v1/apps/:app_id/config",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let config = state
        .db
        .get_import_config(app_id)
        .map_err(internal_error)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(config))
}

async fn list_app_env_vars(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AppEnvVarListResponse>, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Read,
        "GET",
        "/api/v1/apps/:app_id/env",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let items = state
        .db
        .list_app_env_vars(app_id)
        .map_err(internal_error)?
        .into_iter()
        .map(|item| AppEnvVarSummary {
            app_id: item.app_id,
            key: item.key,
            value: item.value,
            updated_at_unix_ms: item.updated_at_unix_ms,
        })
        .collect::<Vec<_>>();

    Ok(Json(AppEnvVarListResponse { items }))
}

async fn upsert_app_env_var(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpsertAppEnvVarRequest>,
) -> Result<(StatusCode, Json<AppEnvVarSummary>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "PUT",
        "/api/v1/apps/:app_id/env",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let key = payload.key.trim();
    if !is_valid_env_var_key(key) || payload.value.len() > 16_384 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let record = state
        .db
        .upsert_app_env_var(app_id, key, &payload.value, now_unix_ms())
        .map_err(internal_error)?;

    Ok((
        StatusCode::OK,
        Json(AppEnvVarSummary {
            app_id: record.app_id,
            key: record.key,
            value: record.value,
            updated_at_unix_ms: record.updated_at_unix_ms,
        }),
    ))
}

async fn delete_app_env_var(
    AxumPath((app_id, key)): AxumPath<(Uuid, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "DELETE",
        "/api/v1/apps/:app_id/env/:key",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let key = key.trim();
    if !is_valid_env_var_key(key) {
        return Err(StatusCode::BAD_REQUEST);
    }

    if state
        .db
        .delete_app_env_var(app_id, key, now_unix_ms())
        .map_err(internal_error)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn create_domain(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateDomainRequest>,
) -> Result<(StatusCode, Json<DomainSummary>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "POST",
        "/api/v1/apps/:app_id/domains",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let tls_mode = payload.tls_mode.as_deref().unwrap_or("managed").to_string();
    if tls_mode != "managed" && tls_mode != "custom" {
        return Err(StatusCode::BAD_REQUEST);
    }
    if tls_mode == "custom" && (payload.cert_path.is_none() || payload.key_path.is_none()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let domain = normalize_domain(&payload.domain);
    if domain.is_empty() || is_reserved_control_plane_domain(&domain) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let summary = state
        .db
        .create_domain(
            app_id,
            &CreateDomainRequest {
                domain,
                tls_mode: Some(tls_mode),
                cert_path: payload.cert_path.clone(),
                key_path: payload.key_path.clone(),
            },
            now_unix_ms(),
        )
        .map_err(internal_error)?
        .ok_or(StatusCode::CONFLICT)?;

    if let Err(error) = sync_caddyfile_from_db(&state.db) {
        warn!(%error, "failed syncing caddyfile after domain change");
    }

    Ok((StatusCode::CREATED, Json(summary)))
}

async fn list_domains(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DomainListResponse>, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Read,
        "GET",
        "/api/v1/apps/:app_id/domains",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let items = state.db.list_domains(app_id).map_err(internal_error)?;
    Ok(Json(DomainListResponse { items }))
}

#[derive(Debug, Serialize)]
struct StreamLogsEventPayload {
    deployment_id: Option<Uuid>,
    logs: String,
    reset: bool,
}

fn encode_stream_logs_event_payload(
    deployment_id: Option<Uuid>,
    logs: &str,
    reset: bool,
) -> Result<String> {
    serde_json::to_string(&StreamLogsEventPayload {
        deployment_id,
        logs: logs.to_string(),
        reset,
    })
    .context("failed encoding logs stream payload")
}

async fn stream_app_logs(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>>, StatusCode>
{
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Read,
        "GET",
        "/api/v1/apps/:app_id/logs/stream",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let state_for_stream = state.clone();
    let mut active_deployment_id: Option<Uuid> = None;
    let mut last_log_id: Option<i64> = None;
    let mut sent_empty = false;
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_millis(750))).filter_map(
        move |_| {
            let latest_deployment_id = match state_for_stream
                .db
                .latest_deployment_id_for_app(app_id)
            {
                Ok(value) => value,
                Err(error) => {
                    warn!(%error, %app_id, "failed fetching latest deployment for log stream");
                    return None;
                }
            };

            if latest_deployment_id != active_deployment_id {
                active_deployment_id = latest_deployment_id;
                last_log_id = None;
                sent_empty = false;
            }

            let Some(deployment_id) = active_deployment_id else {
                if sent_empty {
                    return None;
                }
                sent_empty = true;
                let payload = match encode_stream_logs_event_payload(None, "", true) {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        warn!(%error, %app_id, "failed encoding empty logs stream payload");
                        return None;
                    }
                };
                return Some(Ok(Event::default().event("logs").data(payload)));
            };

            let rows = match state_for_stream
                .db
                .deployment_log_rows_since(deployment_id, last_log_id)
            {
                Ok(value) => value,
                Err(error) => {
                    warn!(%error, %app_id, %deployment_id, "failed loading incremental deployment logs");
                    return None;
                }
            };

            if rows.is_empty() {
                if sent_empty {
                    return None;
                }
                sent_empty = true;
                let payload = match encode_stream_logs_event_payload(Some(deployment_id), "", true)
                {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        warn!(
                            %error,
                            %app_id,
                            %deployment_id,
                            "failed encoding empty deployment logs payload"
                        );
                        return None;
                    }
                };
                return Some(Ok(Event::default().event("logs").data(payload)));
            }

            sent_empty = false;
            let reset = last_log_id.is_none();
            last_log_id = rows.last().map(|row| row.id);
            let logs = rows
                .into_iter()
                .map(|row| row.message)
                .collect::<Vec<_>>()
                .join("\n");
            let payload =
                match encode_stream_logs_event_payload(Some(deployment_id), &logs, reset) {
                Ok(encoded) => encoded,
                Err(error) => {
                    warn!(%error, %app_id, %deployment_id, "failed encoding logs stream payload");
                    return None;
                }
            };

            Some(Ok(Event::default().event("logs").data(payload)))
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().text("keepalive")))
}

async fn get_deployment_logs(
    AxumPath((app_id, deployment_id)): AxumPath<(Uuid, Uuid)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DeploymentLogsResponse>, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Read,
        "GET",
        "/api/v1/apps/:app_id/deployments/:deployment_id/logs",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }
    if !state
        .db
        .deployment_belongs_to_app(app_id, deployment_id)
        .map_err(internal_error)?
    {
        return Err(StatusCode::NOT_FOUND);
    }

    let logs = state
        .db
        .deployment_log_output(deployment_id)
        .map_err(internal_error)?;
    Ok(Json(DeploymentLogsResponse {
        deployment_id,
        logs,
    }))
}

async fn create_deployment(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateDeploymentRequest>,
) -> Result<(StatusCode, Json<CreateDeploymentAccepted>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Deploy,
        "POST",
        "/api/v1/apps/:app_id/deployments",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let mut payload = payload;
    if payload.source_ref.is_none() {
        if let Some(config) = state.db.get_import_config(app_id).map_err(internal_error)? {
            if let Some(branch) = config.source.branch {
                payload.source_ref = Some(format!("import:{branch}"));
            }
        }
    }
    if payload.commit_sha.is_none() {
        if let Some(source_ref) = payload.source_ref.as_deref() {
            if let Some(commit) = source_ref.strip_prefix("github:") {
                payload.commit_sha = Some(commit.to_string());
            }
        }
    }
    if payload.image_ref.is_none() {
        let tag = payload
            .commit_sha
            .as_deref()
            .map(|sha| sha.chars().take(12).collect::<String>())
            .unwrap_or_else(|| "latest".to_string());
        payload.image_ref = Some(format!("rustploy/{app_id}:{tag}"));
    }

    let accepted = state
        .db
        .create_deployment_and_enqueue_job(app_id, &payload, now_unix_ms(), state.job_max_attempts)
        .map_err(internal_error)?;

    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn rollback_deployment(
    AxumPath(app_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<CreateDeploymentAccepted>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Deploy,
        "POST",
        "/api/v1/apps/:app_id/rollback",
    )?;

    if !state.db.app_exists(app_id).map_err(internal_error)? {
        return Err(StatusCode::NOT_FOUND);
    }

    let Some(source_ref) = state
        .db
        .latest_healthy_source_ref(app_id)
        .map_err(internal_error)?
    else {
        return Err(StatusCode::CONFLICT);
    };

    let accepted = state
        .db
        .create_deployment_and_enqueue_job(
            app_id,
            &CreateDeploymentRequest {
                source_ref: Some(source_ref),
                image_ref: Some(format!("rustploy/{app_id}:rollback")),
                commit_sha: None,
                simulate_failures: Some(0),
                force_rebuild: Some(false),
            },
            now_unix_ms(),
            state.job_max_attempts,
        )
        .map_err(internal_error)?;

    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn list_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<TokenListResponse>, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "GET",
        "/api/v1/tokens",
    )?;
    let items = state.db.list_api_tokens().map_err(internal_error)?;
    Ok(Json(TokenListResponse { items }))
}

async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "POST",
        "/api/v1/tokens",
    )?;

    let name = payload.name.trim();
    if name.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let scope_mask = parse_scope_mask(&payload.scopes)?;
    let now = now_unix_ms();
    let expires_at = payload
        .expires_in_seconds
        .map(|ttl| now.saturating_add(ttl.saturating_mul(1_000)));
    let secret = format!("rp_{}.{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let hash = token_hash(&secret);

    let summary = state
        .db
        .create_api_token(name, &hash, scope_mask, expires_at, now)
        .map_err(internal_error)?;

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            token: secret,
            summary,
        }),
    ))
}

async fn revoke_token(
    AxumPath(token_id): AxumPath<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "DELETE",
        "/api/v1/tokens/:token_id",
    )?;

    let revoked = state
        .db
        .revoke_api_token(token_id, now_unix_ms())
        .map_err(internal_error)?;
    if revoked {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn list_agents(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AgentListResponse>, StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Read,
        "GET",
        "/api/v1/agents",
    )?;

    let items = state
        .db
        .list_agents(now_unix_ms(), state.online_window_ms)
        .map_err(internal_error)?;

    Ok(Json(AgentListResponse { items }))
}

async fn register_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AgentRegisterRequest>,
) -> Result<(StatusCode, Json<AgentRegistered>), StatusCode> {
    let traceparent = headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    info!(agent_id = %payload.agent_id, traceparent, "agent register request");
    authorize_agent_request(&headers, &state.agent_shared_token)?;

    let created = state
        .db
        .register_agent(&payload, now_unix_ms())
        .map_err(internal_error)?;

    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };

    Ok((status, Json(AgentRegistered::from_created(created))))
}

async fn agent_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AgentHeartbeat>,
) -> Result<(StatusCode, Json<HeartbeatAccepted>), StatusCode> {
    let traceparent = headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    info!(agent_id = %payload.agent_id, traceparent, "agent heartbeat request");
    authorize_agent_request(&headers, &state.agent_shared_token)?;

    state
        .db
        .record_heartbeat(&payload, now_unix_ms())
        .map_err(internal_error)?;

    Ok((StatusCode::ACCEPTED, Json(HeartbeatAccepted::yes())))
}

fn verify_github_signature(
    headers: &HeaderMap,
    body: &[u8],
    maybe_secret: Option<&str>,
) -> Result<(), StatusCode> {
    let Some(secret) = maybe_secret else {
        return Ok(());
    };

    let signature_header = headers
        .get(GITHUB_SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let signature_hex = signature_header
        .strip_prefix("sha256=")
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let signature = hex::decode(signature_hex).map_err(|_| StatusCode::UNAUTHORIZED)?;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| StatusCode::UNAUTHORIZED)?;
    mac.update(body);

    mac.verify_slice(&signature)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

fn sync_caddyfile_from_db(db: &Database) -> Result<()> {
    let domains = db.list_all_domains()?;
    let apps = db.list_apps()?;
    let runtimes = db.list_app_runtimes()?;
    let runtime_by_app: HashMap<Uuid, AppRuntimeRoute> = runtimes
        .into_iter()
        .map(|runtime| (runtime.app_id, runtime))
        .collect();
    let path =
        std::env::var("RUSTPLOY_CADDYFILE_PATH").unwrap_or_else(|_| "data/Caddyfile".to_string());
    let upstream = std::env::var("RUSTPLOY_UPSTREAM_ADDR")
        .unwrap_or_else(|_| DEFAULT_CADDY_UPSTREAM.to_string());
    if let Some(parent) = Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating {}", parent.display()))?;
        }
    }

    let mut content = String::new();
    content.push_str(&format!(":8080 {{\n  reverse_proxy {upstream}\n}}\n\n"));
    content.push_str(&format!(":80 {{\n  reverse_proxy {upstream}\n}}\n\n"));
    content.push_str(&format!(
        "{CONTROL_PLANE_HOST} {{\n  reverse_proxy {upstream}\n}}\n\n"
    ));
    content.push_str(&format!(
        "{CONTROL_PLANE_ALT_HOST} {{\n  reverse_proxy {upstream}\n}}\n\n"
    ));

    for app in apps {
        let Some(runtime) = runtime_by_app.get(&app.id) else {
            continue;
        };
        let slug = localhost_slug(&app.name);
        if slug.is_empty() {
            continue;
        }
        content.push_str(&format!("{slug}.localhost {{\n"));
        content.push_str(&format!(
            "  reverse_proxy {}:{}\n",
            runtime.upstream_host, runtime.upstream_port
        ));
        content.push_str("}\n\n");
    }

    for domain in domains {
        let normalized = normalize_domain(&domain.domain);
        if normalized.is_empty()
            || normalized == CONTROL_PLANE_HOST
            || normalized == CONTROL_PLANE_ALT_HOST
            || normalized.ends_with(".localhost")
        {
            continue;
        }
        content.push_str(&format!("{} {{\n", normalized));
        if domain.tls_mode == "custom" {
            if let (Some(cert), Some(key)) =
                (domain.cert_path.as_deref(), domain.key_path.as_deref())
            {
                content.push_str(&format!("  tls {cert} {key}\n"));
            }
        }
        if let Some(runtime) = runtime_by_app.get(&domain.app_id) {
            content.push_str(&format!(
                "  reverse_proxy {}:{}\n",
                runtime.upstream_host, runtime.upstream_port
            ));
        } else {
            content.push_str(&format!("  reverse_proxy {upstream}\n"));
        }
        content.push_str("}\n\n");
    }
    content.push_str(&format!(
        "*.localhost {{\n  reverse_proxy {upstream}\n}}\n\n"
    ));

    fs::write(&path, content).with_context(|| format!("failed writing caddyfile at {path}"))?;
    Ok(())
}

fn authorize_api_request(
    state: &AppState,
    headers: &HeaderMap,
    required_scope: RequiredScope,
    method: &str,
    path: &str,
) -> Result<(), StatusCode> {
    state.check_rate_limit(
        &format!("api:{method}:{path}"),
        state.api_rate_limit_per_minute,
    )?;

    let has_tokens = state.db.has_unrevoked_tokens().map_err(internal_error)?;
    let has_users = state.db.has_users().map_err(internal_error)?;
    if !has_tokens && !has_users {
        return Ok(());
    }

    if let Some(token) = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        let now = now_unix_ms();
        let maybe_token = state
            .db
            .authenticate_api_token(&token_hash(token), now, method, path)
            .map_err(internal_error)?;
        let authenticated = maybe_token.ok_or(StatusCode::UNAUTHORIZED)?;
        return if scope_allows(authenticated.scope_mask, required_scope) {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        };
    }

    let session = authorize_session_request(state, headers)?;
    if let Some(session) = session {
        if session_scope_allows(&session.role, required_scope) {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn authorize_session_request(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<AuthenticatedSession>, StatusCode> {
    let Some(session_token) = extract_cookie_value(headers, SESSION_COOKIE_NAME) else {
        return Ok(None);
    };
    state
        .db
        .authenticate_session(&token_hash(&session_token), now_unix_ms())
        .map_err(internal_error)
}

fn extract_cookie_value(headers: &HeaderMap, key: &str) -> Option<String> {
    let cookie = headers.get(COOKIE_HEADER)?.to_str().ok()?;
    for part in cookie.split(';') {
        let trimmed = part.trim();
        let (k, v) = trimmed.split_once('=')?;
        if k == key {
            return Some(v.to_string());
        }
    }
    None
}

fn session_scope_allows(role: &str, required_scope: RequiredScope) -> bool {
    match role {
        "owner" => true,
        "maintainer" => matches!(required_scope, RequiredScope::Read | RequiredScope::Deploy),
        "viewer" => matches!(required_scope, RequiredScope::Read),
        _ => false,
    }
}

fn scope_allows(scope_mask: u8, required_scope: RequiredScope) -> bool {
    if scope_mask & SCOPE_ADMIN != 0 {
        return true;
    }
    match required_scope {
        RequiredScope::Read => scope_mask & SCOPE_READ != 0,
        RequiredScope::Deploy => scope_mask & SCOPE_DEPLOY != 0,
        RequiredScope::Admin => false,
    }
}

fn parse_scope_mask(scopes: &[String]) -> Result<u8, StatusCode> {
    if scopes.is_empty() {
        return Ok(SCOPE_ADMIN | SCOPE_DEPLOY | SCOPE_READ);
    }

    let mut mask = 0u8;
    for scope in scopes {
        match scope.trim().to_ascii_lowercase().as_str() {
            "read" => {
                mask |= SCOPE_READ;
            }
            "deploy" => {
                mask |= SCOPE_DEPLOY | SCOPE_READ;
            }
            "admin" => {
                mask |= SCOPE_ADMIN | SCOPE_DEPLOY | SCOPE_READ;
            }
            _ => return Err(StatusCode::BAD_REQUEST),
        }
    }
    Ok(mask)
}

fn scopes_from_mask(scope_mask: u8) -> Vec<String> {
    let mut scopes = Vec::new();
    if scope_mask & SCOPE_ADMIN != 0 {
        scopes.push("admin".to_string());
    }
    if scope_mask & SCOPE_DEPLOY != 0 {
        scopes.push("deploy".to_string());
    }
    if scope_mask & SCOPE_READ != 0 {
        scopes.push("read".to_string());
    }
    scopes
}

fn token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn clone_repository(clone_url: &str, branch: &str, target_path: &Path) -> Result<(), StatusCode> {
    let output = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(branch)
        .arg(clone_url)
        .arg(target_path)
        .output()
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if output.status.success() {
        Ok(())
    } else {
        Err(StatusCode::BAD_GATEWAY)
    }
}

#[derive(Clone, Copy, Debug)]
enum CommandOutputChannel {
    Stdout,
    Stderr,
}

fn run_command_with_live_output(
    mut command: Command,
    description: &str,
    redactions: &[String],
    mut on_line: impl FnMut(CommandOutputChannel, &str),
) -> Result<String> {
    let safe_description = redact_log_line(description, redactions);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("failed starting command: {safe_description}"))?;

    let stdout = child
        .stdout
        .take()
        .context("failed attaching command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed attaching command stderr")?;
    const MAX_IN_FLIGHT_LINES: usize = 1_024;
    const MAX_RETAINED_LINES: usize = 512;
    let (sender, receiver) =
        mpsc::sync_channel::<(CommandOutputChannel, String)>(MAX_IN_FLIGHT_LINES);
    let stdout_reader =
        spawn_command_output_reader(stdout, CommandOutputChannel::Stdout, sender.clone());
    let stderr_reader =
        spawn_command_output_reader(stderr, CommandOutputChannel::Stderr, sender.clone());
    drop(sender);

    let mut rendered_lines = VecDeque::with_capacity(MAX_RETAINED_LINES);
    for (channel, line) in receiver {
        on_line(channel, &line);
        if rendered_lines.len() == MAX_RETAINED_LINES {
            rendered_lines.pop_front();
        }
        let entry = match channel {
            CommandOutputChannel::Stdout => line,
            CommandOutputChannel::Stderr => format!("stderr: {line}"),
        };
        rendered_lines.push_back(entry);
    }

    let status = child
        .wait()
        .with_context(|| format!("failed waiting for command: {safe_description}"))?;
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();

    let rendered = redact_log_line(
        &rendered_lines.into_iter().collect::<Vec<_>>().join("\n"),
        redactions,
    );
    if status.success() {
        Ok(rendered)
    } else if rendered.is_empty() {
        warn!(
            command = %safe_description,
            "command failed with no output; output was streamed to deployment logs"
        );
        anyhow::bail!("command failed (redacted) with no output");
    } else {
        warn!(
            command = %safe_description,
            "command failed; output was streamed to deployment logs"
        );
        anyhow::bail!("command failed (redacted); command output was streamed to deployment logs");
    }
}

fn spawn_command_output_reader<R: Read + Send + 'static>(
    reader: R,
    channel: CommandOutputChannel,
    sender: mpsc::SyncSender<(CommandOutputChannel, String)>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let value = String::from_utf8_lossy(&buf)
                        .trim_end_matches(['\n', '\r'])
                        .to_string();
                    if sender.send((channel, value)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ =
                        sender.send((channel, format!("<failed reading command output: {error}>")));
                    break;
                }
            }
        }
    })
}

fn append_streamed_command_log(
    db: &Database,
    deployment_id: Uuid,
    source: &str,
    line: &str,
    redactions: &[String],
) {
    if line.trim().is_empty() {
        return;
    }
    let sanitized = redact_log_line(line, redactions);
    let message = format!("{source}: {sanitized}");
    if let Err(error) = db.append_deployment_log(deployment_id, &message, now_unix_ms()) {
        warn!(%error, "failed persisting streamed deployment log line");
    }
}

fn redact_log_line(line: &str, redactions: &[String]) -> String {
    let mut sanitized = line.to_string();
    let mut candidates = redactions
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| value.len() >= 4)
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    candidates.dedup();

    for value in candidates {
        if sanitized.contains(&value) {
            sanitized = sanitized.replace(&value, "***");
        }
    }
    sanitized
}

fn credentials_fragment_from_url(url: &str) -> Option<String> {
    let (_, remainder) = url.split_once("://")?;
    let (authority, _) = remainder.split_once('@')?;
    if authority.is_empty() || authority.contains('/') {
        return None;
    }
    Some(authority.to_string())
}

fn clone_repository_for_deploy_with_live_logs(
    clone_url: &str,
    branch: &str,
    target_path: &Path,
    db: &Database,
    deployment_id: Uuid,
    redactions: &[String],
) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(branch)
        .arg(clone_url)
        .arg(target_path);
    run_command_with_live_output(
        command,
        &format!("git clone {clone_url}@{branch}"),
        redactions,
        |channel, line| {
            let source = match channel {
                CommandOutputChannel::Stdout => "git",
                CommandOutputChannel::Stderr => "git/stderr",
            };
            append_streamed_command_log(db, deployment_id, source, line, redactions);
        },
    )?;
    Ok(())
}

fn checkout_commit_for_deploy_with_live_logs(
    repo_path: &Path,
    commit_sha: &str,
    db: &Database,
    deployment_id: Uuid,
    redactions: &[String],
) -> Result<()> {
    let mut fetch = Command::new("git");
    fetch
        .arg("-C")
        .arg(repo_path)
        .arg("fetch")
        .arg("--depth")
        .arg("1")
        .arg("origin")
        .arg(commit_sha);
    run_command_with_live_output(
        fetch,
        &format!("git fetch {commit_sha}"),
        redactions,
        |channel, line| {
            let source = match channel {
                CommandOutputChannel::Stdout => "git",
                CommandOutputChannel::Stderr => "git/stderr",
            };
            append_streamed_command_log(db, deployment_id, source, line, redactions);
        },
    )?;

    let mut checkout = Command::new("git");
    checkout
        .arg("-C")
        .arg(repo_path)
        .arg("checkout")
        .arg("--detach")
        .arg(commit_sha);
    run_command_with_live_output(
        checkout,
        &format!("git checkout {commit_sha}"),
        redactions,
        |channel, line| {
            let source = match channel {
                CommandOutputChannel::Stdout => "git",
                CommandOutputChannel::Stderr => "git/stderr",
            };
            append_streamed_command_log(db, deployment_id, source, line, redactions);
        },
    )?;
    Ok(())
}

fn runtime_root_path() -> PathBuf {
    let configured = std::env::var("RUSTPLOY_RUNTIME_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/runtime"));
    if configured.is_absolute() {
        configured
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(configured)
    }
}

fn runtime_checkout_path(root: &Path, app_id: Uuid) -> PathBuf {
    root.join("checkouts").join(app_id.to_string())
}

fn runtime_upstream_host() -> String {
    std::env::var("RUSTPLOY_RUNTIME_UPSTREAM_HOST")
        .unwrap_or_else(|_| "host.docker.internal".to_string())
}

fn compose_project_name(app_id: Uuid) -> String {
    let simple = app_id.simple().to_string();
    let short = &simple[..12];
    format!("rustploy-{short}")
}

fn detect_app_service_port(compose: &ComposeSummary, app_service: &str) -> Option<u16> {
    compose
        .services
        .iter()
        .find(|service| service.name == app_service)
        .and_then(|service| {
            service
                .ports
                .iter()
                .find_map(|port| parse_compose_container_port(port))
        })
}

fn parse_compose_container_port(value: &str) -> Option<u16> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_protocol = trimmed.split('/').next().unwrap_or(trimmed);
    let tail = without_protocol
        .rsplit(':')
        .next()
        .unwrap_or(without_protocol);
    if tail.contains('-') {
        return None;
    }
    tail.trim().parse::<u16>().ok()
}

fn write_runtime_compose_file(
    source_compose_path: &Path,
    runtime_compose_path: &Path,
    app_service: &str,
    app_port: u16,
    app_env: &HashMap<String, String>,
) -> Result<()> {
    let raw = fs::read_to_string(source_compose_path).with_context(|| {
        format!(
            "failed reading source compose file {}",
            source_compose_path.display()
        )
    })?;
    let mut parsed: serde_yaml::Value = serde_yaml::from_str(&raw)
        .with_context(|| format!("invalid yaml in {}", source_compose_path.display()))?;
    let services = parsed
        .get_mut("services")
        .and_then(serde_yaml::Value::as_mapping_mut)
        .context("compose file has no services mapping")?;

    for (service_name, service_config) in services.iter_mut() {
        let Some(service_name) = service_name.as_str() else {
            continue;
        };
        let Some(service_map) = service_config.as_mapping_mut() else {
            continue;
        };
        service_map.remove(serde_yaml::Value::String("volumes".to_string()));
        if service_name == app_service {
            let mut port = serde_yaml::Mapping::new();
            port.insert(
                serde_yaml::Value::String("target".to_string()),
                serde_yaml::Value::Number(serde_yaml::Number::from(i64::from(app_port))),
            );
            port.insert(
                serde_yaml::Value::String("published".to_string()),
                serde_yaml::Value::String("0".to_string()),
            );
            service_map.insert(
                serde_yaml::Value::String("ports".to_string()),
                serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(port)]),
            );
            merge_service_environment(service_map, app_env);
        } else {
            service_map.remove(serde_yaml::Value::String("ports".to_string()));
        }
    }

    let rendered = serde_yaml::to_string(&parsed)
        .with_context(|| format!("failed serializing {}", source_compose_path.display()))?;
    if let Some(parent) = runtime_compose_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating {}", parent.display()))?;
        }
    }
    fs::write(runtime_compose_path, rendered).with_context(|| {
        format!(
            "failed writing runtime compose file {}",
            runtime_compose_path.display()
        )
    })?;
    Ok(())
}

fn merge_service_environment(
    service_map: &mut serde_yaml::Mapping,
    app_env: &HashMap<String, String>,
) {
    if app_env.is_empty() {
        return;
    }

    let env_key = serde_yaml::Value::String("environment".to_string());
    let existing = service_map.remove(&env_key);
    let mut merged = serde_yaml::Mapping::new();

    if let Some(existing) = existing {
        merge_existing_environment(&mut merged, existing);
    }
    for (key, value) in app_env {
        merged.insert(
            serde_yaml::Value::String(key.clone()),
            serde_yaml::Value::String(value.clone()),
        );
    }

    service_map.insert(env_key, serde_yaml::Value::Mapping(merged));
}

fn merge_existing_environment(target: &mut serde_yaml::Mapping, existing: serde_yaml::Value) {
    match existing {
        serde_yaml::Value::Mapping(map) => {
            for (key, value) in map {
                let Some(normalized_key) = yaml_scalar_to_string(&key) else {
                    continue;
                };
                target.insert(serde_yaml::Value::String(normalized_key), value);
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                if let Some((key, value)) = parse_compose_env_entry(&item) {
                    target.insert(key, value);
                }
            }
        }
        _ => {}
    }
}

fn parse_compose_env_entry(
    item: &serde_yaml::Value,
) -> Option<(serde_yaml::Value, serde_yaml::Value)> {
    let text = item.as_str()?;
    if let Some((key, value)) = text.split_once('=') {
        return Some((
            serde_yaml::Value::String(key.trim().to_string()),
            serde_yaml::Value::String(value.to_string()),
        ));
    }
    Some((
        serde_yaml::Value::String(text.trim().to_string()),
        serde_yaml::Value::Null,
    ))
}

fn compose_base_command() -> Result<Vec<String>> {
    let docker_compose = Command::new("docker")
        .arg("compose")
        .arg("version")
        .output();
    if matches!(docker_compose, Ok(output) if output.status.success()) {
        return Ok(vec!["docker".to_string(), "compose".to_string()]);
    }

    let docker_compose_bin = Command::new("docker-compose").arg("version").output();
    if matches!(docker_compose_bin, Ok(output) if output.status.success()) {
        return Ok(vec!["docker-compose".to_string()]);
    }

    anyhow::bail!(
        "docker compose is not available; install docker compose and expose /var/run/docker.sock"
    );
}

fn run_compose_command(
    working_dir: &Path,
    project_name: &str,
    files: &[&Path],
    args: &[&str],
) -> Result<String> {
    let base = compose_base_command()?;
    let mut cmd = Command::new(&base[0]);
    if base.len() > 1 {
        cmd.arg(&base[1]);
    }
    cmd.current_dir(working_dir)
        .arg("--project-name")
        .arg(project_name);
    for file in files {
        cmd.arg("-f").arg(file);
    }
    for arg in args {
        cmd.arg(arg);
    }

    let output = cmd.output().context("failed executing compose command")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(
            "compose command failed ({}): {} {}",
            args.join(" "),
            stdout,
            stderr
        );
    }
}

fn run_compose_command_with_live_logs(
    working_dir: &Path,
    project_name: &str,
    files: &[&Path],
    args: &[&str],
    db: &Database,
    deployment_id: Uuid,
    redactions: &[String],
) -> Result<String> {
    let base = compose_base_command()?;
    let mut command = Command::new(&base[0]);
    if base.len() > 1 {
        command.arg(&base[1]);
    }
    command
        .current_dir(working_dir)
        .arg("--project-name")
        .arg(project_name);
    for file in files {
        command.arg("-f").arg(file);
    }
    for arg in args {
        command.arg(arg);
    }

    run_command_with_live_output(
        command,
        &format!("compose {}", args.join(" ")),
        redactions,
        |channel, line| {
            let source = match channel {
                CommandOutputChannel::Stdout => "compose",
                CommandOutputChannel::Stderr => "compose/stderr",
            };
            append_streamed_command_log(db, deployment_id, source, line, redactions);
        },
    )
}

fn resolve_compose_service_port(
    working_dir: &Path,
    project_name: &str,
    files: &[&Path],
    service_name: &str,
    target_port: u16,
) -> Result<u16> {
    let target = target_port.to_string();
    let output = run_compose_command(
        working_dir,
        project_name,
        files,
        &["port", service_name, &target],
    )?;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(port) = parse_published_port(trimmed) {
            return Ok(port);
        }
    }
    anyhow::bail!(
        "unable to resolve published port for compose service {}:{}",
        service_name,
        target_port
    );
}

fn parse_published_port(value: &str) -> Option<u16> {
    value.rsplit(':').next()?.trim().parse::<u16>().ok()
}

fn wait_for_compose_service_ready(
    working_dir: &Path,
    project_name: &str,
    files: &[&Path],
    service_name: &str,
    deployment_id: Uuid,
    now_unix_ms: u64,
    db: &Database,
) -> Result<()> {
    let container_id = run_compose_command(
        working_dir,
        project_name,
        files,
        &["ps", "-q", service_name],
    )?;
    let container_id = container_id
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|value| value.trim().to_string())
        .context("compose service did not return a container id")?;

    let timeout_secs = read_env_u64("RUSTPLOY_DEPLOY_READY_TIMEOUT_SECS", 90)?;
    for _ in 0..timeout_secs {
        let output = Command::new("docker")
            .arg("inspect")
            .arg("--format")
            .arg("{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}} {{.State.ExitCode}}")
            .arg(&container_id)
            .output()
            .context("failed inspecting compose service container")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            anyhow::bail!("docker inspect failed for service {service_name}: {stderr}");
        }
        let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let mut parts = state.split_whitespace();
        let status = parts.next().unwrap_or_default();
        let health = parts.next().unwrap_or_default();

        if status == "running" && (health.is_empty() || health == "healthy") {
            return Ok(());
        }
        if status == "exited" || status == "dead" || health == "unhealthy" {
            anyhow::bail!(
                "compose service {} is not healthy (status={}, health={})",
                service_name,
                status,
                health
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    db.append_deployment_log(
        deployment_id,
        "service readiness timed out; marking deployment as failed",
        now_unix_ms,
    )?;
    anyhow::bail!("compose service readiness timed out for {}", service_name);
}

fn read_manifest(
    repo_path: &Path,
) -> std::result::Result<Option<RustployManifest>, Vec<ManifestValidationError>> {
    let manifest_path = repo_path.join("rustploy.yaml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&manifest_path).map_err(|error| {
        vec![ManifestValidationError {
            field: "rustploy.yaml".to_string(),
            message: format!("unable to read manifest: {error}"),
        }]
    })?;
    let manifest = parse_manifest_yaml(&content)?;
    if manifest.version.unwrap_or(1) != 1 {
        return Err(vec![ManifestValidationError {
            field: "version".to_string(),
            message: "version must be 1".to_string(),
        }]);
    }
    Ok(Some(manifest))
}

fn parse_manifest_yaml(
    content: &str,
) -> std::result::Result<RustployManifest, Vec<ManifestValidationError>> {
    let mut manifest = RustployManifest {
        version: None,
        app: None,
        build: None,
        dependencies: None,
    };
    let mut errors = Vec::new();

    let mut section = String::new();
    let mut dependency_key = String::new();

    for raw_line in content.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len().saturating_sub(line.trim_start().len());
        match indent {
            0 => {
                dependency_key.clear();
                if let Some(value) = parse_yaml_value(trimmed, "version") {
                    match value.parse::<u32>() {
                        Ok(parsed) => {
                            manifest.version = Some(parsed);
                        }
                        Err(_) => errors.push(ManifestValidationError {
                            field: "version".to_string(),
                            message: format!("invalid version value: {value}"),
                        }),
                    }
                } else if trimmed == "app:" {
                    section = "app".to_string();
                } else if trimmed == "build:" {
                    section = "build".to_string();
                } else if trimmed == "dependencies:" {
                    section = "dependencies".to_string();
                } else {
                    section.clear();
                }
            }
            2 => {
                if section == "app" {
                    if let Some(value) = parse_yaml_value(trimmed, "name") {
                        let app = manifest.app.get_or_insert(ManifestAppConfig { name: None });
                        app.name = Some(value.to_string());
                    }
                } else if section == "build" {
                    if let Some(value) = parse_yaml_value(trimmed, "mode") {
                        if value != "auto" && value != "dockerfile" && value != "compose" {
                            errors.push(ManifestValidationError {
                                field: "build.mode".to_string(),
                                message: format!(
                                    "invalid mode '{value}', expected auto, dockerfile, or compose"
                                ),
                            });
                        }
                        let build = manifest
                            .build
                            .get_or_insert(ManifestBuildConfig { mode: None });
                        build.mode = Some(value.to_string());
                    }
                } else if section == "dependencies" {
                    if trimmed == "postgres:" {
                        dependency_key = "postgres".to_string();
                        let dependencies =
                            manifest.dependencies.get_or_insert(ManifestDependencies {
                                postgres: None,
                                redis: None,
                            });
                        dependencies.postgres = Some(ManifestDependencyToggle { enabled: None });
                    } else if trimmed == "redis:" {
                        dependency_key = "redis".to_string();
                        let dependencies =
                            manifest.dependencies.get_or_insert(ManifestDependencies {
                                postgres: None,
                                redis: None,
                            });
                        dependencies.redis = Some(ManifestDependencyToggle { enabled: None });
                    } else {
                        dependency_key.clear();
                    }
                }
            }
            4 => {
                if section == "dependencies" {
                    if let Some(value) = parse_yaml_value(trimmed, "enabled") {
                        let enabled = match parse_yaml_bool(value) {
                            Ok(enabled) => enabled,
                            Err(_) => {
                                errors.push(ManifestValidationError {
                                    field: format!("dependencies.{dependency_key}.enabled"),
                                    message: format!(
                                        "invalid boolean '{value}', expected true or false"
                                    ),
                                });
                                continue;
                            }
                        };
                        let dependencies =
                            manifest.dependencies.get_or_insert(ManifestDependencies {
                                postgres: None,
                                redis: None,
                            });
                        if dependency_key == "postgres" {
                            let toggle = dependencies
                                .postgres
                                .get_or_insert(ManifestDependencyToggle { enabled: None });
                            toggle.enabled = Some(enabled);
                        } else if dependency_key == "redis" {
                            let toggle = dependencies
                                .redis
                                .get_or_insert(ManifestDependencyToggle { enabled: None });
                            toggle.enabled = Some(enabled);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if errors.is_empty() {
        Ok(manifest)
    } else {
        Err(errors)
    }
}

fn parse_yaml_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}:");
    line.strip_prefix(&prefix)
        .map(|value| value.trim_matches('"').trim())
}

fn parse_yaml_bool(value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => anyhow::bail!("invalid boolean in manifest"),
    }
}

fn merge_dependency_profile(
    requested: Option<DependencyProfile>,
    manifest: Option<&RustployManifest>,
    compose: Option<&ComposeSummary>,
) -> Option<DependencyProfile> {
    let manifest_postgres = manifest
        .and_then(|value| value.dependencies.as_ref())
        .and_then(|value| value.postgres.as_ref())
        .and_then(|value| value.enabled);
    let manifest_redis = manifest
        .and_then(|value| value.dependencies.as_ref())
        .and_then(|value| value.redis.as_ref())
        .and_then(|value| value.enabled);
    let compose_profile = infer_dependency_profile_from_compose(compose);

    let postgres = requested
        .as_ref()
        .and_then(|value| value.postgres)
        .or(manifest_postgres)
        .or(compose_profile.postgres);
    let redis = requested
        .as_ref()
        .and_then(|value| value.redis)
        .or(manifest_redis)
        .or(compose_profile.redis);

    if postgres.is_none() && redis.is_none() {
        None
    } else {
        Some(DependencyProfile { postgres, redis })
    }
}

fn infer_dependency_profile_from_compose(compose: Option<&ComposeSummary>) -> DependencyProfile {
    let mut postgres = None;
    let mut redis = None;

    if let Some(compose) = compose {
        for service in &compose.services {
            let image = service
                .image
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if image.contains("postgis") || image.contains("postgres") {
                postgres = Some(true);
            }
            if image.contains("redis") {
                redis = Some(true);
            }
        }
    }

    DependencyProfile { postgres, redis }
}

fn compose_image_for_dependency(
    compose: Option<&ComposeSummary>,
    service_type: &str,
) -> Option<String> {
    let compose = compose?;
    for service in &compose.services {
        let image = service.image.as_deref()?;
        let normalized = image.to_ascii_lowercase();
        match service_type {
            "postgres" => {
                if normalized.contains("postgres") || normalized.contains("postgis") {
                    return Some(image.to_string());
                }
            }
            "redis" => {
                if normalized.contains("redis") {
                    return Some(image.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn detect_build_profile(
    repo_path: &Path,
    compose: Option<&ComposeSummary>,
) -> Result<DetectionResult, StatusCode> {
    let dockerfile_present = repo_path.join("Dockerfile").exists();
    let package_json_path = repo_path.join("package.json");
    if !package_json_path.exists() {
        if compose.is_some() {
            return Ok(DetectionResult {
                framework: "compose".to_string(),
                package_manager: "none".to_string(),
                lockfile: None,
                build_profile: "compose-generic-v1".to_string(),
                dockerfile_present,
            });
        }
        if dockerfile_present {
            return Ok(DetectionResult {
                framework: "docker".to_string(),
                package_manager: "none".to_string(),
                lockfile: None,
                build_profile: "dockerfile-generic-v1".to_string(),
                dockerfile_present,
            });
        }
        return Err(StatusCode::BAD_REQUEST);
    }

    let package_json_raw =
        fs::read_to_string(package_json_path).map_err(|_| StatusCode::BAD_REQUEST)?;
    let package_json: serde_json::Value =
        serde_json::from_str(&package_json_raw).map_err(|_| StatusCode::BAD_REQUEST)?;

    let dependencies = package_json
        .get("dependencies")
        .and_then(|value| value.as_object())
        .cloned();
    let dev_dependencies = package_json
        .get("devDependencies")
        .and_then(|value| value.as_object())
        .cloned();
    let scripts = package_json
        .get("scripts")
        .and_then(|value| value.as_object())
        .cloned();

    let next_present = dependencies
        .as_ref()
        .is_some_and(|value| value.contains_key("next"))
        || dev_dependencies
            .as_ref()
            .is_some_and(|value| value.contains_key("next"));
    let has_build_script = scripts
        .as_ref()
        .is_some_and(|value| value.contains_key("build"));
    let has_start_script = scripts
        .as_ref()
        .is_some_and(|value| value.contains_key("start"));

    let (package_manager, lockfile) = if repo_path.join("pnpm-lock.yaml").exists() {
        ("pnpm".to_string(), Some("pnpm-lock.yaml".to_string()))
    } else if repo_path.join("yarn.lock").exists() {
        ("yarn".to_string(), Some("yarn.lock".to_string()))
    } else if repo_path.join("package-lock.json").exists() {
        ("npm".to_string(), Some("package-lock.json".to_string()))
    } else {
        ("npm".to_string(), None)
    };

    let (framework, build_profile) = if next_present && has_build_script && has_start_script {
        ("nextjs".to_string(), "nextjs-standalone-v1".to_string())
    } else {
        ("node".to_string(), "node-generic-v1".to_string())
    };

    Ok(DetectionResult {
        framework,
        package_manager,
        lockfile,
        build_profile,
        dockerfile_present,
    })
}

fn read_compose_summary(repo_path: &Path) -> Result<Option<ComposeSummary>> {
    for filename in [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let compose_path = repo_path.join(filename);
        if !compose_path.exists() {
            continue;
        }

        let raw = fs::read_to_string(&compose_path)
            .with_context(|| format!("unable to read {}", compose_path.display()))?;
        let parsed: ComposeFileRaw = serde_yaml::from_str(&raw)
            .with_context(|| format!("invalid yaml in {}", compose_path.display()))?;
        if parsed.services.is_empty() {
            anyhow::bail!("compose file has no services");
        }

        let mut services = Vec::new();
        for (service_name, service) in parsed.services {
            services.push(ComposeServiceSummary {
                name: service_name,
                image: service.image,
                build: service.build.is_some(),
                depends_on: parse_compose_depends_on(service.depends_on.as_ref()),
                ports: parse_compose_ports(service.ports.as_ref()),
                profiles: service.profiles.unwrap_or_default(),
            });
        }
        services.sort_by(|left, right| left.name.cmp(&right.name));
        let app_service = detect_compose_app_service(&services);

        return Ok(Some(ComposeSummary {
            file: filename.to_string(),
            app_service,
            services,
        }));
    }

    Ok(None)
}

fn parse_compose_depends_on(value: Option<&serde_yaml::Value>) -> Vec<String> {
    let mut depends_on = Vec::new();
    let Some(value) = value else {
        return depends_on;
    };

    match value {
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                if let Some(name) = item.as_str() {
                    depends_on.push(name.to_string());
                }
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for (name, _settings) in map {
                if let Some(name) = name.as_str() {
                    depends_on.push(name.to_string());
                }
            }
        }
        _ => {}
    }

    depends_on.sort();
    depends_on.dedup();
    depends_on
}

fn parse_compose_ports(value: Option<&Vec<serde_yaml::Value>>) -> Vec<String> {
    let mut ports = Vec::new();
    let Some(value) = value else {
        return ports;
    };

    for item in value {
        match item {
            serde_yaml::Value::String(raw) => ports.push(raw.to_string()),
            serde_yaml::Value::Number(number) => ports.push(number.to_string()),
            serde_yaml::Value::Mapping(map) => {
                let published = map
                    .get(serde_yaml::Value::String("published".to_string()))
                    .and_then(yaml_scalar_to_string);
                let target = map
                    .get(serde_yaml::Value::String("target".to_string()))
                    .and_then(yaml_scalar_to_string);

                if let Some(target_value) = target.as_deref() {
                    if let Some(published_value) = published.as_deref() {
                        ports.push(format!("{published_value}:{target_value}"));
                    } else {
                        ports.push(target_value.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    ports
}

fn parse_compose_env_files(value: Option<&serde_yaml::Value>) -> Vec<String> {
    let mut files = Vec::new();
    let Some(value) = value else {
        return files;
    };
    match value {
        serde_yaml::Value::String(single) => files.push(single.to_string()),
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    files.push(text.to_string());
                }
            }
        }
        _ => {}
    }
    files
}

fn ensure_compose_env_files(repo_path: &Path, compose_filename: &str) -> Result<Vec<PathBuf>> {
    let compose_path = repo_path.join(compose_filename);
    let raw = fs::read_to_string(&compose_path)
        .with_context(|| format!("unable to read {}", compose_path.display()))?;
    let parsed: ComposeFileRaw = serde_yaml::from_str(&raw)
        .with_context(|| format!("invalid yaml in {}", compose_path.display()))?;

    let mut created = Vec::new();
    for (_service_name, service) in parsed.services {
        for env_file in parse_compose_env_files(service.env_file.as_ref()) {
            let trimmed = env_file.trim();
            if trimmed.is_empty() {
                continue;
            }
            let relative_path = PathBuf::from(trimmed);
            if relative_path.is_absolute() {
                continue;
            }
            let env_path = repo_path.join(relative_path);
            if env_path.exists() {
                continue;
            }
            if let Some(parent) = env_path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!("failed creating env_file directory {}", parent.display())
                    })?;
                }
            }
            let seed = env_example_contents(&env_path).unwrap_or_default();
            fs::write(&env_path, seed)
                .with_context(|| format!("failed creating env file {}", env_path.display()))?;
            created.push(env_path);
        }
    }

    Ok(created)
}

fn env_example_contents(env_path: &Path) -> Option<String> {
    let parent = env_path.parent()?;
    let filename = env_path.file_name()?.to_string_lossy();
    let mut candidate = parent.join(format!("{filename}.example"));
    if candidate.exists() {
        return fs::read_to_string(candidate).ok();
    }
    if filename == ".env" {
        candidate = parent.join(".env.example");
        if candidate.exists() {
            return fs::read_to_string(candidate).ok();
        }
    }
    None
}

fn yaml_scalar_to_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(text) => Some(text.to_string()),
        serde_yaml::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn detect_compose_app_service(services: &[ComposeServiceSummary]) -> Option<String> {
    if let Some(service) = services.iter().find(|service| service.name == "web") {
        return Some(service.name.clone());
    }
    if let Some(service) = services.iter().find(|service| service.name == "app") {
        return Some(service.name.clone());
    }
    if let Some(service) = services
        .iter()
        .find(|service| service.build && !service.ports.is_empty())
    {
        return Some(service.name.clone());
    }
    if let Some(service) = services.iter().find(|service| service.build) {
        return Some(service.name.clone());
    }
    if let Some(service) = services.iter().find(|service| !service.ports.is_empty()) {
        return Some(service.name.clone());
    }
    services.first().map(|service| service.name.clone())
}

fn authorize_agent_request(
    headers: &HeaderMap,
    expected_token: &Option<String>,
) -> Result<(), StatusCode> {
    match expected_token {
        None => Ok(()),
        Some(expected) => {
            let token = headers
                .get(AGENT_TOKEN_HEADER)
                .and_then(|value| value.to_str().ok())
                .ok_or(StatusCode::UNAUTHORIZED)?;

            if token == expected {
                Ok(())
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
    }
}

fn internal_error(error: anyhow::Error) -> StatusCode {
    error!(
        error = %redact_secrets(&error.to_string()),
        "request failed"
    );
    StatusCode::INTERNAL_SERVER_ERROR
}

fn redact_secrets(input: &str) -> String {
    input
        .replace("Authorization: Bearer ", "Authorization: Bearer [redacted]")
        .replace("rp_", "rp_[redacted]")
        .replace("sess_", "sess_[redacted]")
        .replace("reset_", "reset_[redacted]")
}

fn api_error(
    status: StatusCode,
    code: &str,
    message: &str,
    details: Vec<ApiErrorDetail>,
) -> ApiJsonError {
    (
        status,
        Json(ApiErrorResponse {
            error: ApiError {
                code: code.to_string(),
                message: message.to_string(),
                details,
            },
        }),
    )
}

fn status_to_api_error(status: StatusCode) -> ApiJsonError {
    match status {
        StatusCode::UNAUTHORIZED => api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication is required",
            Vec::new(),
        ),
        StatusCode::FORBIDDEN => api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "token scope does not allow this operation",
            Vec::new(),
        ),
        StatusCode::NOT_FOUND => api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "resource not found",
            Vec::new(),
        ),
        StatusCode::CONFLICT => api_error(
            StatusCode::CONFLICT,
            "conflict",
            "resource conflict",
            Vec::new(),
        ),
        StatusCode::BAD_GATEWAY => api_error(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            "failed to access upstream repository",
            Vec::new(),
        ),
        _ => api_error(
            status,
            "invalid_request",
            "request failed validation",
            Vec::new(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    fn test_db_path() -> String {
        let path = std::env::temp_dir().join(format!("rustploy-test-{}.db", Uuid::new_v4()));
        path.to_string_lossy().to_string()
    }

    fn cleanup_db(path: &str) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}-wal"));
        let _ = std::fs::remove_file(format!("{path}-shm"));
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn token_protected_agent_routes_require_header() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(
            &db_path,
            Some("top-secret".to_string()),
        ));

        let register = json!({
            "agent_id": Uuid::new_v4(),
            "agent_version": "0.1.0"
        });

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/register")
                    .header("content-type", "application/json")
                    .body(Body::from(register.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/register")
                    .header("content-type", "application/json")
                    .header(AGENT_TOKEN_HEADER, "top-secret")
                    .body(Body::from(register.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(authorized.status(), StatusCode::CREATED);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn github_webhook_rejects_invalid_signature_and_queues_on_valid_signature() {
        let db_path = test_db_path();
        let secret = "webhook-secret".to_string();
        let state = AppState::from_config(AppStateConfig {
            db_path: db_path.clone(),
            agent_shared_token: None,
            github_webhook_secret: Some(secret.clone()),
            session_ttl_ms: DEFAULT_SESSION_TTL_MS,
            password_reset_ttl_ms: DEFAULT_PASSWORD_RESET_TTL_MS,
            bootstrap_admin_email: None,
            bootstrap_admin_password: None,
            online_window_ms: DEFAULT_ONLINE_WINDOW_MS,
            reconciler_enabled: false,
            reconciler_interval_ms: 1,
            job_backoff_base_ms: 1,
            job_backoff_max_ms: 100,
            job_max_attempts: DEFAULT_JOB_MAX_ATTEMPTS,
            api_rate_limit_per_minute: 10_000,
            webhook_rate_limit_per_minute: 10_000,
        })
        .unwrap();
        let app = create_router(state.clone());

        let create_app_payload = json!({ "name": "github-app" });
        let create_app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(create_app_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_app_response.status(), StatusCode::CREATED);
        let app_body = to_bytes(create_app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let connect_payload = json!({
            "owner": "acme",
            "repo": "demo",
            "branch": "main",
            "installation_id": 42
        });
        let connect_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/github"))
                    .header("content-type", "application/json")
                    .body(Body::from(connect_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(connect_response.status(), StatusCode::OK);

        let webhook_body = json!({
            "ref": "refs/heads/main",
            "after": "abcdef123456",
            "repository": {
                "name": "demo",
                "owner": { "login": "acme" }
            }
        })
        .to_string();

        let bad_signature = "sha256=deadbeef";
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/integrations/github/webhook")
                    .header("content-type", "application/json")
                    .header(GITHUB_EVENT_HEADER, "push")
                    .header(GITHUB_SIGNATURE_HEADER, bad_signature)
                    .body(Body::from(webhook_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let signature = signed_signature(&secret, webhook_body.as_bytes());
        let accepted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/integrations/github/webhook")
                    .header("content-type", "application/json")
                    .header(GITHUB_EVENT_HEADER, "push")
                    .header(GITHUB_SIGNATURE_HEADER, signature)
                    .body(Body::from(webhook_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);

        assert!(state.run_reconciler_once().unwrap());

        let deployments = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let deployments_body = to_bytes(deployments.into_body(), usize::MAX).await.unwrap();
        let deployments_json: Value = serde_json::from_slice(&deployments_body).unwrap();
        assert_eq!(deployments_json["items"][0]["status"], json!("healthy"));

        cleanup_db(&db_path);
    }

    fn signed_signature(secret: &str, payload: &[u8]) -> String {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let result = mac.finalize().into_bytes();
        format!("sha256={}", hex::encode(result))
    }

    #[tokio::test]
    async fn api_tokens_enforce_scopes_and_revocation() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let create_admin = json!({
            "name": "admin",
            "scopes": ["admin"]
        });
        let admin_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tokens")
                    .header("content-type", "application/json")
                    .body(Body::from(create_admin.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(admin_response.status(), StatusCode::CREATED);
        let admin_body = to_bytes(admin_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let admin_json: Value = serde_json::from_slice(&admin_body).unwrap();
        let admin_token = admin_json["token"].as_str().unwrap().to_string();

        let create_read = json!({
            "name": "readonly",
            "scopes": ["read"]
        });
        let read_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tokens")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION_HEADER, format!("Bearer {admin_token}"))
                    .body(Body::from(create_read.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read_response.status(), StatusCode::CREATED);
        let read_body = to_bytes(read_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let read_json: Value = serde_json::from_slice(&read_body).unwrap();
        let read_token = read_json["token"].as_str().unwrap().to_string();
        let read_token_id = read_json["summary"]["id"].as_str().unwrap().to_string();

        let create_app_payload = json!({ "name": "token-app" });
        let create_app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION_HEADER, format!("Bearer {admin_token}"))
                    .body(Body::from(create_app_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_app_response.status(), StatusCode::CREATED);
        let app_body = to_bytes(create_app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let list_apps = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/apps")
                    .header(AUTHORIZATION_HEADER, format!("Bearer {read_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_apps.status(), StatusCode::OK);

        let deploy_payload = json!({ "source_ref": "main", "simulate_failures": 0 });
        let deploy_denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION_HEADER, format!("Bearer {read_token}"))
                    .body(Body::from(deploy_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deploy_denied.status(), StatusCode::FORBIDDEN);

        let revoke = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/tokens/{read_token_id}"))
                    .header(AUTHORIZATION_HEADER, format!("Bearer {admin_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoke.status(), StatusCode::NO_CONTENT);

        let revoked_list = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/apps")
                    .header(AUTHORIZATION_HEADER, format!("Bearer {read_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked_list.status(), StatusCode::UNAUTHORIZED);

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn session_auth_allows_api_access_when_bootstrap_admin_exists() {
        let db_path = test_db_path();
        let state = AppState::from_config(AppStateConfig {
            db_path: db_path.clone(),
            agent_shared_token: None,
            github_webhook_secret: None,
            session_ttl_ms: DEFAULT_SESSION_TTL_MS,
            password_reset_ttl_ms: DEFAULT_PASSWORD_RESET_TTL_MS,
            bootstrap_admin_email: Some("admin@localhost".to_string()),
            bootstrap_admin_password: Some("password123".to_string()),
            online_window_ms: DEFAULT_ONLINE_WINDOW_MS,
            reconciler_enabled: false,
            reconciler_interval_ms: 1,
            job_backoff_base_ms: 1,
            job_backoff_max_ms: 100,
            job_max_attempts: DEFAULT_JOB_MAX_ATTEMPTS,
            api_rate_limit_per_minute: 10_000,
            webhook_rate_limit_per_minute: 10_000,
        })
        .unwrap();
        let app = create_router(state);

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"secure-app"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"admin@localhost","password":"password123"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let set_cookie = login
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        let cookie_pair = set_cookie.split(';').next().unwrap().to_string();

        let created = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .header("cookie", cookie_pair)
                    .body(Body::from(r#"{"name":"secure-app"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn sessions_expire_and_logout_revokes_access() {
        let db_path = test_db_path();
        let state = AppState::from_config(AppStateConfig {
            db_path: db_path.clone(),
            agent_shared_token: None,
            github_webhook_secret: None,
            session_ttl_ms: 5,
            password_reset_ttl_ms: DEFAULT_PASSWORD_RESET_TTL_MS,
            bootstrap_admin_email: Some("admin@localhost".to_string()),
            bootstrap_admin_password: Some("password123".to_string()),
            online_window_ms: DEFAULT_ONLINE_WINDOW_MS,
            reconciler_enabled: false,
            reconciler_interval_ms: 1,
            job_backoff_base_ms: 1,
            job_backoff_max_ms: 100,
            job_max_attempts: DEFAULT_JOB_MAX_ATTEMPTS,
            api_rate_limit_per_minute: 10_000,
            webhook_rate_limit_per_minute: 10_000,
        })
        .unwrap();
        let app = create_router(state);

        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"admin@localhost","password":"password123"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let set_cookie = login
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        let cookie_pair = set_cookie.split(';').next().unwrap().to_string();

        tokio::time::sleep(Duration::from_millis(10)).await;
        let expired = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .header("cookie", cookie_pair.clone())
                    .body(Body::from(r#"{"name":"should-fail"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);

        let login_two = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"admin@localhost","password":"password123"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login_two.status(), StatusCode::OK);
        let set_cookie_two = login_two
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        let cookie_pair_two = set_cookie_two.split(';').next().unwrap().to_string();

        let logout = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header("cookie", cookie_pair_two.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);

        let revoked = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .header("cookie", cookie_pair_two)
                    .body(Body::from(r#"{"name":"should-fail-too"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn import_detects_nextjs_and_pnpm_and_applies_manifest_name() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(
            repo_path.join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.2.0"
  }
}"#,
        )
        .unwrap();
        std::fs::write(repo_path.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
        std::fs::write(
            repo_path.join("rustploy.yaml"),
            "version: 1\napp:\n  name: manifest-name\nbuild:\n  mode: auto\ndependencies:\n  postgres:\n    enabled: true\n",
        )
        .unwrap();

        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "next-app",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": {
                "branch": "main",
                "commit_sha": null
            },
            "build_mode": "auto",
            "dependency_profile": {
                "postgres": null,
                "redis": null
            }
        });

        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import_response.status(), StatusCode::CREATED);
        let import_body = to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let import_json: Value = serde_json::from_slice(&import_body).unwrap();
        assert_eq!(import_json["detection"]["framework"], json!("nextjs"));
        assert_eq!(import_json["detection"]["package_manager"], json!("pnpm"));
        assert_eq!(import_json["app"]["name"], json!("manifest-name"));

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn nextjs_import_and_deploy_detects_all_package_managers() {
        let db_path = test_db_path();
        let state = AppState::for_tests(&db_path, None);
        let app = create_router(state.clone());

        for (lockfile_name, lockfile_content, expected_manager) in [
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n", "pnpm"),
            ("yarn.lock", "# yarn lock\n", "yarn"),
            (
                "package-lock.json",
                "{\n  \"lockfileVersion\": 3\n}\n",
                "npm",
            ),
        ] {
            let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&repo_path).unwrap();
            std::fs::write(
                repo_path.join("package.json"),
                r#"{
  "name": "demo",
  "scripts": {
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.2.0"
  }
}"#,
            )
            .unwrap();
            std::fs::write(repo_path.join(lockfile_name), lockfile_content).unwrap();

            run_git(&repo_path, &["init", "-b", "main"]);
            run_git(&repo_path, &["config", "user.email", "test@example.com"]);
            run_git(&repo_path, &["config", "user.name", "Test User"]);
            run_git(&repo_path, &["add", "."]);
            run_git(&repo_path, &["commit", "-m", "init"]);

            let import_payload = json!({
                "repository": {
                    "provider": "github",
                    "owner": "acme",
                    "name": format!("next-{expected_manager}"),
                    "clone_url": repo_path.to_string_lossy(),
                    "default_branch": "main"
                },
                "source": { "branch": "main" },
                "build_mode": "auto"
            });
            let import_response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/apps/import")
                        .header("content-type", "application/json")
                        .body(Body::from(import_payload.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(import_response.status(), StatusCode::CREATED);
            let import_body = to_bytes(import_response.into_body(), usize::MAX)
                .await
                .unwrap();
            let import_json: Value = serde_json::from_slice(&import_body).unwrap();
            assert_eq!(
                import_json["detection"]["package_manager"],
                json!(expected_manager)
            );
            let app_id = import_json["app"]["id"].as_str().unwrap();

            let deploy_response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/apps/{app_id}/deployments"))
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(deploy_response.status(), StatusCode::ACCEPTED);
            let deploy_body = to_bytes(deploy_response.into_body(), usize::MAX)
                .await
                .unwrap();
            let deploy_json: Value = serde_json::from_slice(&deploy_body).unwrap();
            let deployment_id = deploy_json["deployment_id"].as_str().unwrap();

            assert!(state.run_reconciler_once().unwrap());

            let logs_response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/api/v1/apps/{app_id}/deployments/{deployment_id}/logs"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(logs_response.status(), StatusCode::OK);
            let logs_body = to_bytes(logs_response.into_body(), usize::MAX)
                .await
                .unwrap();
            let logs_json: Value = serde_json::from_slice(&logs_body).unwrap();
            let logs = logs_json["logs"].as_str().unwrap();
            assert!(logs.contains(&format!("package_manager={expected_manager}")));

            let _ = std::fs::remove_dir_all(&repo_path);
        }

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn import_invalid_manifest_returns_structured_validation_error() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(
            repo_path.join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.2.0"
  }
}"#,
        )
        .unwrap();
        std::fs::write(repo_path.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
        std::fs::write(
            repo_path.join("rustploy.yaml"),
            "version: 1\nbuild:\n  mode: invalid\n",
        )
        .unwrap();

        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "next-app",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": {
                "branch": "main"
            }
        });

        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import_response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"]["code"], json!("invalid_manifest"));
        assert_eq!(payload["error"]["details"][0]["field"], json!("build.mode"));

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn compose_import_detects_services_and_postgis_dependency() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(
            repo_path.join("docker-compose.yml"),
            r#"services:
  db:
    image: imresamu/postgis:16-3.4
    ports:
      - "5432:5432"
  web:
    build: .
    ports:
      - "8000:8000"
    depends_on:
      db:
        condition: service_healthy
"#,
        )
        .unwrap();
        std::fs::write(repo_path.join("Dockerfile"), "FROM python:3.12-slim\n").unwrap();

        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "covachapp",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": { "branch": "main" }
        });
        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import_response.status(), StatusCode::CREATED);
        let import_body = to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let import_json: Value = serde_json::from_slice(&import_body).unwrap();
        assert_eq!(import_json["detection"]["framework"], json!("compose"));
        assert_eq!(import_json["compose"]["app_service"], json!("web"));

        let app_id = import_json["app"]["id"].as_str().unwrap();
        let config_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/config"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(config_response.status(), StatusCode::OK);
        let config_body = to_bytes(config_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let config_json: Value = serde_json::from_slice(&config_body).unwrap();
        assert_eq!(config_json["build_mode"], json!("compose"));
        assert_eq!(config_json["dependency_profile"]["postgres"], json!(true));
        assert_eq!(config_json["compose"]["file"], json!("docker-compose.yml"));
        assert_eq!(config_json["compose"]["services"][0]["name"], json!("db"));
        assert_eq!(
            config_json["compose"]["services"][0]["image"],
            json!("imresamu/postgis:16-3.4")
        );

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn import_is_idempotent_for_existing_app_name() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(repo_path.join("Dockerfile"), "FROM python:3.12-slim\n").unwrap();
        std::fs::write(
            repo_path.join("docker-compose.yml"),
            "services:\n  web:\n    build: .\n    ports:\n      - \"8000:8000\"\n",
        )
        .unwrap();
        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "covachapp",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": { "branch": "main" }
        });

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
        let first_json: Value = serde_json::from_slice(&first_body).unwrap();
        let first_app_id = first_json["app"]["id"].as_str().unwrap().to_string();

        let second = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::CREATED);
        let second_body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
        let second_json: Value = serde_json::from_slice(&second_body).unwrap();
        let second_app_id = second_json["app"]["id"].as_str().unwrap().to_string();
        assert_eq!(first_app_id, second_app_id);

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn effective_config_endpoint_returns_imported_values() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(
            repo_path.join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.2.0"
  }
}"#,
        )
        .unwrap();
        std::fs::write(repo_path.join("yarn.lock"), "# yarn lock\n").unwrap();
        std::fs::write(
            repo_path.join("rustploy.yaml"),
            "version: 1\nbuild:\n  mode: dockerfile\ndependencies:\n  postgres:\n    enabled: true\n  redis:\n    enabled: false\n",
        )
        .unwrap();
        std::fs::write(repo_path.join("Dockerfile"), "FROM node:20-alpine\n").unwrap();

        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "next-app",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": {
                "branch": "main"
            }
        });
        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import_response.status(), StatusCode::CREATED);
        let import_body = to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let import_json: Value = serde_json::from_slice(&import_body).unwrap();
        let app_id = import_json["app"]["id"].as_str().unwrap();

        let config_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/config"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(config_response.status(), StatusCode::OK);
        let config_body = to_bytes(config_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let config_json: Value = serde_json::from_slice(&config_body).unwrap();
        assert_eq!(config_json["build_mode"], json!("dockerfile"));
        assert_eq!(config_json["detection"]["package_manager"], json!("yarn"));
        assert_eq!(config_json["dependency_profile"]["postgres"], json!(true));
        assert_eq!(config_json["dependency_profile"]["redis"], json!(false));

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn domains_can_be_created_and_listed() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"domain-app"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let app_body = to_bytes(app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let create_domain = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/domains"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"domain":"example.test","tls_mode":"managed"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_domain.status(), StatusCode::CREATED);

        let list_domains = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/domains"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_domains.status(), StatusCode::OK);
        let body = to_bytes(list_domains.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["items"][0]["domain"], json!("example.test"));

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn app_env_vars_can_be_upserted_listed_and_deleted() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"env-app"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let app_body = to_bytes(app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let upsert = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/apps/{app_id}/env"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"key":"DATABASE_URL","value":"postgres://covach:covach@db:5432/covach"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(upsert.status(), StatusCode::OK);

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/env"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let listed_body = to_bytes(listed.into_body(), usize::MAX).await.unwrap();
        let listed_json: Value = serde_json::from_slice(&listed_body).unwrap();
        assert_eq!(listed_json["items"][0]["key"], json!("DATABASE_URL"));
        assert_eq!(
            listed_json["items"][0]["value"],
            json!("postgres://covach:covach@db:5432/covach")
        );

        let deleted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/apps/{app_id}/env/DATABASE_URL"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

        let listed_after_delete = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/env"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed_after_delete.status(), StatusCode::OK);
        let listed_after_delete_body = to_bytes(listed_after_delete.into_body(), usize::MAX)
            .await
            .unwrap();
        let listed_after_delete_json: Value =
            serde_json::from_slice(&listed_after_delete_body).unwrap();
        assert_eq!(listed_after_delete_json["items"], json!([]));

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn localhost_domain_is_rejected_for_apps() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"reject-localhost"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let app_body = to_bytes(app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let create_domain = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/domains"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"domain":"localhost","tls_mode":"managed"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_domain.status(), StatusCode::BAD_REQUEST);

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn app_localhost_host_serves_app_frontdoor() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let create_app = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"smoke-next"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_app.status(), StatusCode::CREATED);

        let frontdoor = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(HOST_HEADER, "smoke-next.localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(frontdoor.status(), StatusCode::OK);
        let body = to_bytes(frontdoor.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("smoke-next"));
        assert!(html.contains("App edge endpoint"));

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn deployment_without_source_ref_uses_import_branch() {
        let db_path = test_db_path();
        let app = create_router(AppState::for_tests(&db_path, None));

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(
            repo_path.join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.2.0"
  }
}"#,
        )
        .unwrap();
        std::fs::write(repo_path.join("package-lock.json"), "{}\n").unwrap();
        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "next-app",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": {
                "branch": "main"
            }
        });
        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import_response.status(), StatusCode::CREATED);
        let import_body = to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let import_json: Value = serde_json::from_slice(&import_body).unwrap();
        let app_id = import_json["app"]["id"].as_str().unwrap();

        let deploy_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deploy_response.status(), StatusCode::ACCEPTED);

        let deployments = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let deployments_body = to_bytes(deployments.into_body(), usize::MAX).await.unwrap();
        let deployments_json: Value = serde_json::from_slice(&deployments_body).unwrap();
        assert_eq!(
            deployments_json["items"][0]["source_ref"],
            json!("import:main")
        );

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn deployment_logs_endpoint_returns_build_step_output() {
        let db_path = test_db_path();
        let state = AppState::for_tests(&db_path, None);
        let app = create_router(state.clone());

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(
            repo_path.join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.2.0"
  }
}"#,
        )
        .unwrap();
        std::fs::write(repo_path.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "next-app",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": {
                "branch": "main"
            }
        });
        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let import_body = to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let import_json: Value = serde_json::from_slice(&import_body).unwrap();
        let app_id = import_json["app"]["id"].as_str().unwrap();

        let deploy_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let deploy_body = to_bytes(deploy_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let deploy_json: Value = serde_json::from_slice(&deploy_body).unwrap();
        let deployment_id = deploy_json["deployment_id"].as_str().unwrap();

        assert!(state.run_reconciler_once().unwrap());

        let logs_response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/apps/{app_id}/deployments/{deployment_id}/logs"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logs_response.status(), StatusCode::OK);
        let logs_body = to_bytes(logs_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let logs_json: Value = serde_json::from_slice(&logs_body).unwrap();
        let logs = logs_json["logs"].as_str().unwrap();
        assert!(logs.contains("deployment started"));
        assert!(logs.contains("package_manager=pnpm"));
        assert!(logs.contains("deployment healthy"));

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn managed_dependencies_are_logged_and_gated() {
        let db_path = test_db_path();
        let state = AppState::for_tests(&db_path, None);
        let app = create_router(state.clone());

        let repo_path = std::env::temp_dir().join(format!("rustploy-repo-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(
            repo_path.join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "build": "next build",
    "start": "next start"
  },
  "dependencies": {
    "next": "14.2.0"
  }
}"#,
        )
        .unwrap();
        std::fs::write(
            repo_path.join("rustploy.yaml"),
            "version: 1\ndependencies:\n  postgres:\n    enabled: true\n  redis:\n    enabled: true\n",
        )
        .unwrap();
        run_git(&repo_path, &["init", "-b", "main"]);
        run_git(&repo_path, &["config", "user.email", "test@example.com"]);
        run_git(&repo_path, &["config", "user.name", "Test User"]);
        run_git(&repo_path, &["add", "."]);
        run_git(&repo_path, &["commit", "-m", "init"]);

        let import_payload = json!({
            "repository": {
                "provider": "github",
                "owner": "acme",
                "name": "dep-app",
                "clone_url": repo_path.to_string_lossy(),
                "default_branch": "main"
            },
            "source": { "branch": "main" }
        });
        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps/import")
                    .header("content-type", "application/json")
                    .body(Body::from(import_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let import_body = to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let import_json: Value = serde_json::from_slice(&import_body).unwrap();
        let app_id = import_json["app"]["id"].as_str().unwrap();

        let deploy_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let deploy_body = to_bytes(deploy_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let deploy_json: Value = serde_json::from_slice(&deploy_body).unwrap();
        let deployment_id = deploy_json["deployment_id"].as_str().unwrap();

        assert!(state.run_reconciler_once().unwrap());

        let logs_response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/apps/{app_id}/deployments/{deployment_id}/logs"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let logs_body = to_bytes(logs_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let logs_json: Value = serde_json::from_slice(&logs_body).unwrap();
        let logs = logs_json["logs"].as_str().unwrap();
        assert!(logs.contains("managed dependency ready: postgres"));
        assert!(logs.contains("managed dependency ready: redis"));

        let _ = std::fs::remove_dir_all(&repo_path);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn rollback_requeues_latest_healthy_source() {
        let db_path = test_db_path();
        let state = AppState::for_tests(&db_path, None);
        let app = create_router(state.clone());

        let create_app_payload = json!({ "name": "rollback-app" });
        let create_app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(create_app_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let app_body = to_bytes(create_app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let deploy_payload = json!({ "source_ref": "github:abc123", "simulate_failures": 0 });
        let deploy = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .body(Body::from(deploy_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deploy.status(), StatusCode::ACCEPTED);
        assert!(state.run_reconciler_once().unwrap());

        let rollback = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/rollback"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rollback.status(), StatusCode::ACCEPTED);

        let deployments = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let deployments_body = to_bytes(deployments.into_body(), usize::MAX).await.unwrap();
        let deployments_json: Value = serde_json::from_slice(&deployments_body).unwrap();
        assert_eq!(
            deployments_json["items"][0]["source_ref"],
            json!("github:abc123")
        );
        assert_eq!(deployments_json["items"][0]["status"], json!("queued"));

        assert!(state.run_reconciler_once().unwrap());
        let after = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let after_body = to_bytes(after.into_body(), usize::MAX).await.unwrap();
        let after_json: Value = serde_json::from_slice(&after_body).unwrap();
        assert_eq!(after_json["items"][0]["status"], json!("healthy"));

        cleanup_db(&db_path);
    }

    fn run_git(repo_path: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git command failed: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn reconciler_creates_and_executes_deployment_job() {
        let db_path = test_db_path();
        let state = AppState::for_tests(&db_path, None);
        let app = create_router(state.clone());

        let create_app_payload = json!({ "name": "demo-app" });
        let create_app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(create_app_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(create_app_response.status(), StatusCode::CREATED);
        let app_body = to_bytes(create_app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let deploy_payload = json!({
            "source_ref": "main",
            "simulate_failures": 0
        });

        let deployment_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .body(Body::from(deploy_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(deployment_response.status(), StatusCode::ACCEPTED);

        let listed_before = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let before_body = to_bytes(listed_before.into_body(), usize::MAX)
            .await
            .unwrap();
        let before_json: Value = serde_json::from_slice(&before_body).unwrap();
        assert_eq!(before_json["items"][0]["status"], json!("queued"));

        assert!(state.run_reconciler_once().unwrap());

        let listed_after = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let after_body = to_bytes(listed_after.into_body(), usize::MAX)
            .await
            .unwrap();
        let after_json: Value = serde_json::from_slice(&after_body).unwrap();
        assert_eq!(after_json["items"][0]["status"], json!("healthy"));

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn failed_jobs_retry_with_bounded_backoff() {
        let db_path = test_db_path();
        let state = AppState::for_tests(&db_path, None);
        let app = create_router(state.clone());

        let create_app_payload = json!({ "name": "retry-app" });
        let create_app_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(create_app_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let app_body = to_bytes(create_app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let deploy_payload = json!({
            "source_ref": "main",
            "simulate_failures": 1
        });

        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .body(Body::from(deploy_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(state.run_reconciler_once().unwrap());

        let listed_retrying = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let retrying_body = to_bytes(listed_retrying.into_body(), usize::MAX)
            .await
            .unwrap();
        let retrying_json: Value = serde_json::from_slice(&retrying_body).unwrap();
        assert_eq!(retrying_json["items"][0]["status"], json!("retrying"));

        state.force_jobs_due_for_tests();
        assert!(state.run_reconciler_once().unwrap());

        let listed_healthy = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let healthy_body = to_bytes(listed_healthy.into_body(), usize::MAX)
            .await
            .unwrap();
        let healthy_json: Value = serde_json::from_slice(&healthy_body).unwrap();
        assert_eq!(healthy_json["items"][0]["status"], json!("healthy"));

        assert_eq!(compute_backoff_ms(10, 1, 100), 100);
        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn queued_jobs_survive_restart() {
        let db_path = test_db_path();

        let state_one = AppState::for_tests(&db_path, None);
        let app_one = create_router(state_one.clone());

        let create_app_payload = json!({ "name": "persist-app" });
        let create_app_response = app_one
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/apps")
                    .header("content-type", "application/json")
                    .body(Body::from(create_app_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let app_body = to_bytes(create_app_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let app_json: Value = serde_json::from_slice(&app_body).unwrap();
        let app_id = app_json["id"].as_str().unwrap();

        let deploy_payload = json!({ "source_ref": "main", "simulate_failures": 0 });
        app_one
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .header("content-type", "application/json")
                    .body(Body::from(deploy_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        drop(state_one);

        let state_two = AppState::for_tests(&db_path, None);
        let app_two = create_router(state_two.clone());

        let listed_before = app_two
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let before_body = to_bytes(listed_before.into_body(), usize::MAX)
            .await
            .unwrap();
        let before_json: Value = serde_json::from_slice(&before_body).unwrap();
        assert_eq!(before_json["items"][0]["status"], json!("queued"));

        assert!(state_two.run_reconciler_once().unwrap());

        let listed_after = app_two
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/apps/{app_id}/deployments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let after_body = to_bytes(listed_after.into_body(), usize::MAX)
            .await
            .unwrap();
        let after_json: Value = serde_json::from_slice(&after_body).unwrap();
        assert_eq!(after_json["items"][0]["status"], json!("healthy"));

        cleanup_db(&db_path);
    }
}
