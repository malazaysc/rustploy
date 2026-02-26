use std::{
    collections::HashMap,
    convert::Infallible,
    fs,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
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
    AgentSummary, ApiError, ApiErrorDetail, ApiErrorResponse, AppListResponse, AppSummary,
    AuthLoginRequest, AuthSessionResponse, CreateAppRequest, CreateDeploymentAccepted,
    CreateDeploymentRequest, CreateDomainRequest, CreateTokenRequest, CreateTokenResponse,
    DependencyProfile, DeploymentListResponse, DeploymentLogsResponse, DeploymentStatus,
    DeploymentSummary, DetectionResult, DomainListResponse, DomainSummary,
    EffectiveAppConfigResponse, GithubConnectRequest, GithubIntegrationSummary,
    GithubWebhookAccepted, HealthResponse, HeartbeatAccepted, ImportAppRequest, ImportAppResponse,
    NextAction, PasswordResetConfirmRequest, PasswordResetConfirmedResponse, PasswordResetRequest,
    PasswordResetRequestedResponse, RepositoryRef, SourceRef, TokenListResponse, TokenSummary,
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
const SESSION_COOKIE_NAME: &str = "rustploy_session";
const DEFAULT_SESSION_TTL_MS: u64 = 24 * 60 * 60 * 1000;
const DEFAULT_PASSWORD_RESET_TTL_MS: u64 = 15 * 60 * 1000;
const DEFAULT_ADMIN_EMAIL: &str = "admin@localhost";
const DEFAULT_ADMIN_PASSWORD: &str = "admin";
const DEFAULT_CADDY_UPSTREAM: &str = "127.0.0.1:8080";

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
}

#[derive(Debug)]
struct ImportConfigRecord {
    app_id: Uuid,
    repository: RepositoryRef,
    source: SourceRef,
    build_mode: String,
    detection: DetectionResult,
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

        if let Err(error) = sync_caddyfile_from_domains(&state) {
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

        conn.execute(
            "INSERT INTO app_import_configs (
                app_id,
                repo_provider,
                repo_owner,
                repo_name,
                repo_branch,
                build_mode,
                package_manager,
                framework,
                lockfile,
                build_profile,
                dockerfile_present,
                dependency_profile_json,
                manifest_json,
                created_at_unix_ms,
                updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
             ON CONFLICT(app_id) DO UPDATE SET
                repo_provider = excluded.repo_provider,
                repo_owner = excluded.repo_owner,
                repo_name = excluded.repo_name,
                repo_branch = excluded.repo_branch,
                build_mode = excluded.build_mode,
                package_manager = excluded.package_manager,
                framework = excluded.framework,
                lockfile = excluded.lockfile,
                build_profile = excluded.build_profile,
                dockerfile_present = excluded.dockerfile_present,
                dependency_profile_json = excluded.dependency_profile_json,
                manifest_json = excluded.manifest_json,
                updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                config.app_id.to_string(),
                &config.repository.provider,
                &config.repository.owner,
                &config.repository.name,
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
                dependency_profile_json,
                config.manifest_json.as_deref(),
                now,
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
                    repo_branch,
                    build_mode,
                    package_manager,
                    framework,
                    lockfile,
                    build_profile,
                    dockerfile_present,
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
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
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
            repo_branch,
            build_mode,
            package_manager,
            framework,
            lockfile,
            build_profile,
            dockerfile_present,
            dependency_profile_raw,
            manifest_raw,
        )) = row
        else {
            return Ok(None);
        };

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
                clone_url: None,
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
            dependency_profile,
            manifest,
        }))
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
            if let Some(profile) = config.dependency_profile {
                if profile.postgres == Some(true) {
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
                if profile.redis == Some(true) {
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
            if config.build_mode == "dockerfile" && !config.detection.dockerfile_present {
                self.append_deployment_log(
                    payload.deployment_id,
                    "dockerfile mode selected but Dockerfile was not detected",
                    now_unix_ms,
                )?;
                anyhow::bail!("dockerfile mode requires Dockerfile in repository");
            }
            self.append_deployment_log(
                payload.deployment_id,
                "build pipeline step simulation complete",
                now_unix_ms,
            )?;
        }

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

        self.set_deployment_state(
            payload.deployment_id,
            DeploymentStatus::Healthy,
            None,
            now_unix_ms,
        )?;
        self.append_deployment_log(payload.deployment_id, "deployment healthy", now_unix_ms)?;
        Ok(())
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
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT message
                 FROM deployment_logs
                 WHERE deployment_id = ?1
                 ORDER BY created_at_unix_ms ASC, id ASC",
            )
            .context("failed preparing deployment log query")?;
        let mut rows = stmt
            .query(params![deployment_id.to_string()])
            .context("failed querying deployment logs")?;
        let mut lines = Vec::new();
        while let Some(row) = rows.next().context("failed iterating deployment logs")? {
            let message: String = row
                .get(0)
                .context("failed reading deployment log message")?;
            lines.push(message);
        }
        Ok(lines.join("\n"))
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
    ensure_deployments_metadata_columns(conn)?;

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

async fn dashboard_ui(State(state): State<AppState>, headers: HeaderMap) -> Html<&'static str> {
    let session = authorize_session_request(&state, &headers).ok().flatten();
    if session.is_some() {
        Html(DASHBOARD_HTML)
    } else {
        Html(LOGIN_HTML)
    }
}

type ApiJsonError = (StatusCode, Json<ApiErrorResponse>);

const LOGIN_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Rustploy Login</title>
  <style>
    @import url('https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;600;700&display=swap');
    :root { --bg:#0b1220; --card:#14213d; --ink:#e5e7eb; --accent:#fca311; --muted:#9ca3af; }
    * { box-sizing:border-box; font-family:'Space Grotesk',sans-serif; }
    body { margin:0; min-height:100vh; display:grid; place-items:center; background:radial-gradient(circle at 15% 20%,#1f2937,#0b1220 55%); color:var(--ink);}
    .card { width:min(420px,92vw); background:linear-gradient(160deg,#14213d,#1d3557); border:1px solid rgba(255,255,255,.12); border-radius:18px; padding:24px; }
    h1 { margin:0 0 8px; font-size:1.9rem; }
    p { color:var(--muted); margin:0 0 16px; }
    input,button { width:100%; padding:12px 14px; border-radius:12px; border:1px solid rgba(255,255,255,.16); background:#0f172a; color:var(--ink); margin-top:10px; }
    button { background:var(--accent); color:#111827; font-weight:700; border:none; cursor:pointer; }
    .err { margin-top:10px; color:#fca5a5; min-height:1em; }
  </style>
</head>
<body>
  <form class="card" id="login-form">
    <h1>Rustploy</h1>
    <p>Sign in with your owner account.</p>
    <input id="email" type="email" placeholder="admin@localhost" required />
    <input id="password" type="password" placeholder="password" required />
    <button type="submit">Sign In</button>
    <div class="err" id="err"></div>
  </form>
  <script>
    document.getElementById('login-form').addEventListener('submit', async (e) => {
      e.preventDefault();
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
        document.getElementById('err').textContent = 'Invalid credentials';
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
    @import url('https://fonts.googleapis.com/css2?family=Outfit:wght@400;600;700&display=swap');
    :root { --bg:#f5f7fa; --ink:#101827; --accent:#0f766e; --card:#ffffff; --line:#d1d5db; }
    * { box-sizing:border-box; font-family:'Outfit',sans-serif; }
    body { margin:0; color:var(--ink); background:
      radial-gradient(circle at 20% 0%, #a7f3d0 0%, transparent 35%),
      radial-gradient(circle at 80% 100%, #bfdbfe 0%, transparent 40%),
      var(--bg); }
    .wrap { max-width:1050px; margin:0 auto; padding:24px; }
    h1 { margin:0 0 14px; font-size:2rem; }
    .grid { display:grid; gap:14px; grid-template-columns:repeat(auto-fit,minmax(300px,1fr)); }
    .card { background:var(--card); border:1px solid var(--line); border-radius:14px; padding:14px; box-shadow:0 8px 24px rgba(16,24,39,.08); }
    input,button,select { padding:10px 12px; border:1px solid var(--line); border-radius:10px; }
    button { background:var(--accent); color:#fff; border:none; font-weight:700; cursor:pointer; }
    ul { padding-left:18px; }
    pre { white-space:pre-wrap; background:#111827; color:#e5e7eb; border-radius:10px; padding:10px; max-height:220px; overflow:auto; }
  </style>
</head>
<body>
  <div class="wrap">
    <h1>Rustploy Dashboard</h1>
    <div class="grid">
      <section class="card">
        <h3>Create App</h3>
        <input id="app-name" placeholder="my-app" />
        <button onclick="createApp()">Create</button>
      </section>
      <section class="card">
        <h3>Import GitHub Repo</h3>
        <input id="owner" placeholder="owner" />
        <input id="repo" placeholder="repo" />
        <input id="branch" placeholder="main" value="main" />
        <button onclick="importApp()">Import</button>
      </section>
      <section class="card">
        <h3>Apps</h3>
        <ul id="apps"></ul>
      </section>
      <section class="card">
        <h3>Deployments</h3>
        <ul id="deployments"></ul>
        <pre id="logs"></pre>
      </section>
      <section class="card">
        <h3>Domains</h3>
        <input id="domain" placeholder="app.example.com" />
        <button onclick="addDomain()">Add Domain</button>
        <ul id="domains"></ul>
      </section>
      <section class="card">
        <h3>Session</h3>
        <button onclick="logout()">Logout</button>
      </section>
    </div>
  </div>
  <script>
    let selectedApp = null;
    let logsStream = null;
    async function api(path, opts = {}) {
      const res = await fetch(path, { credentials: 'include', ...opts });
      if (res.status === 401) { window.location.reload(); throw new Error('unauthorized'); }
      return res;
    }
    async function refreshApps() {
      const res = await api('/api/v1/apps');
      const data = await res.json();
      const apps = document.getElementById('apps');
      apps.innerHTML = '';
      for (const app of data.items) {
        const li = document.createElement('li');
        li.innerHTML = `<strong>${app.name}</strong> <button onclick="selectApp('${app.id}')">Open</button> <button onclick="deploy('${app.id}')">Deploy</button> <button onclick="rollback('${app.id}')">Rollback</button>`;
        apps.appendChild(li);
      }
      if (!selectedApp && data.items.length > 0) {
        await selectApp(data.items[0].id);
      }
    }
    async function createApp() {
      const name = document.getElementById('app-name').value.trim();
      if (!name) return;
      await api('/api/v1/apps', { method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({name}) });
      refreshApps();
    }
    async function importApp() {
      const owner = document.getElementById('owner').value.trim();
      const repo = document.getElementById('repo').value.trim();
      const branch = document.getElementById('branch').value.trim() || 'main';
      await api('/api/v1/apps/import', {
        method:'POST',
        headers:{'content-type':'application/json'},
        body:JSON.stringify({ repository:{ provider:'github', owner, name:repo, default_branch:branch }, source:{ branch }, build_mode:'auto' })
      });
      refreshApps();
    }
    async function selectApp(appId) {
      selectedApp = appId;
      await refreshDeployments();
      await refreshDomains();
      startLogsStream();
    }
    function startLogsStream() {
      if (logsStream) {
        logsStream.close();
        logsStream = null;
      }
      if (!selectedApp) return;
      logsStream = new EventSource(`/api/v1/apps/${selectedApp}/logs/stream`, { withCredentials: true });
      logsStream.addEventListener('logs', (event) => {
        document.getElementById('logs').textContent = (event.data || '').replaceAll('\\n', '\n') || '(no logs)';
      });
      logsStream.onerror = () => {};
    }
    async function refreshDeployments() {
      if (!selectedApp) return;
      const res = await api(`/api/v1/apps/${selectedApp}/deployments`);
      const data = await res.json();
      const el = document.getElementById('deployments');
      el.innerHTML = '';
      for (const d of data.items) {
        const li = document.createElement('li');
        li.innerHTML = `${d.status} ${d.source_ref || ''} <button onclick="showLogs('${selectedApp}','${d.id}')">Logs</button>`;
        el.appendChild(li);
      }
    }
    async function showLogs(appId, deploymentId) {
      const res = await api(`/api/v1/apps/${appId}/deployments/${deploymentId}/logs`);
      const data = await res.json();
      document.getElementById('logs').textContent = data.logs || '(no logs)';
    }
    async function refreshDomains() {
      if (!selectedApp) return;
      const res = await api(`/api/v1/apps/${selectedApp}/domains`);
      const data = await res.json();
      const el = document.getElementById('domains');
      el.innerHTML = '';
      for (const d of data.items) {
        const li = document.createElement('li');
        li.textContent = `${d.domain} (${d.tls_mode})`;
        el.appendChild(li);
      }
    }
    async function addDomain() {
      if (!selectedApp) return;
      const domain = document.getElementById('domain').value.trim();
      if (!domain) return;
      await api(`/api/v1/apps/${selectedApp}/domains`, {
        method:'POST',
        headers:{'content-type':'application/json'},
        body:JSON.stringify({ domain, tls_mode:'managed' })
      });
      refreshDomains();
    }
    async function deploy(appId) {
      await api(`/api/v1/apps/${appId}/deployments`, { method:'POST', headers:{'content-type':'application/json'}, body:'{}' });
      if (selectedApp === appId) refreshDeployments();
    }
    async function rollback(appId) {
      await api(`/api/v1/apps/${appId}/rollback`, { method:'POST' });
      if (selectedApp === appId) refreshDeployments();
    }
    async function logout() {
      if (logsStream) { logsStream.close(); logsStream = null; }
      await api('/api/v1/auth/logout', { method:'POST' });
      window.location.reload();
    }
    window.addEventListener('beforeunload', () => { if (logsStream) logsStream.close(); });
    refreshApps();
    setInterval(refreshApps, 15000);
    setInterval(refreshDeployments, 4000);
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
        let detection = detect_build_profile(&temp_path).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "build_detection_failed",
                "failed to detect project build profile",
                vec![ApiErrorDetail {
                    field: "package.json".to_string(),
                    message: "missing or invalid package.json".to_string(),
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
            .unwrap_or_else(|| "auto".to_string());
        if build_mode != "auto" && build_mode != "dockerfile" {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "build_mode must be auto or dockerfile",
                vec![ApiErrorDetail {
                    field: "build_mode".to_string(),
                    message: "supported values are auto and dockerfile".to_string(),
                }],
            ));
        }

        let app_name = manifest
            .as_ref()
            .and_then(|value| value.app.as_ref()?.name.clone())
            .unwrap_or_else(|| repo.clone());

        let now = now_unix_ms();
        let app = state
            .db
            .create_app(&app_name, now)
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
            })?;

        let dependency_profile =
            merge_dependency_profile(payload.dependency_profile.clone(), manifest.as_ref());

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
                    dependency_profile,
                    manifest_json,
                },
                now,
            )
            .map_err(|error| status_to_api_error(internal_error(error)))?;

        Ok(ImportAppResponse {
            app: app.clone(),
            detection,
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

    let summary = state
        .db
        .create_domain(
            app_id,
            &CreateDomainRequest {
                domain: payload.domain.trim().to_string(),
                tls_mode: Some(tls_mode),
                cert_path: payload.cert_path.clone(),
                key_path: payload.key_path.clone(),
            },
            now_unix_ms(),
        )
        .map_err(internal_error)?
        .ok_or(StatusCode::CONFLICT)?;

    if let Err(error) = sync_caddyfile_from_domains(&state) {
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
    let stream =
        IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).map(move |_| {
            let logs = state_for_stream
                .db
                .latest_deployment_id_for_app(app_id)
                .ok()
                .flatten()
                .and_then(|deployment_id| {
                    state_for_stream
                        .db
                        .deployment_log_output(deployment_id)
                        .ok()
                })
                .unwrap_or_default();
            Ok(Event::default()
                .event("logs")
                .data(logs.replace('\n', "\\n")))
        });
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

fn sync_caddyfile_from_domains(state: &AppState) -> Result<()> {
    let domains = state.db.list_all_domains()?;
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
    content.push_str("{\n  auto_https on\n}\n\n");
    content.push_str(&format!(":80 {{\n  reverse_proxy {upstream}\n}}\n\n"));
    if domains.is_empty() {
        content.push_str(&format!("localhost {{\n  reverse_proxy {upstream}\n}}\n"));
    } else {
        for domain in domains {
            content.push_str(&format!("{} {{\n", domain.domain));
            if domain.tls_mode == "custom" {
                if let (Some(cert), Some(key)) =
                    (domain.cert_path.as_deref(), domain.key_path.as_deref())
                {
                    content.push_str(&format!("  tls {cert} {key}\n"));
                }
            }
            content.push_str(&format!("  reverse_proxy {upstream}\n"));
            content.push_str("}\n\n");
        }
    }

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
                        if value != "auto" && value != "dockerfile" {
                            errors.push(ManifestValidationError {
                                field: "build.mode".to_string(),
                                message: format!(
                                    "invalid mode '{value}', expected auto or dockerfile"
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
) -> Option<DependencyProfile> {
    let manifest_postgres = manifest
        .and_then(|value| value.dependencies.as_ref())
        .and_then(|value| value.postgres.as_ref())
        .and_then(|value| value.enabled);
    let manifest_redis = manifest
        .and_then(|value| value.dependencies.as_ref())
        .and_then(|value| value.redis.as_ref())
        .and_then(|value| value.enabled);

    let postgres = requested
        .as_ref()
        .and_then(|value| value.postgres)
        .or(manifest_postgres);
    let redis = requested
        .as_ref()
        .and_then(|value| value.redis)
        .or(manifest_redis);

    if postgres.is_none() && redis.is_none() {
        None
    } else {
        Some(DependencyProfile { postgres, redis })
    }
}

fn detect_build_profile(repo_path: &Path) -> Result<DetectionResult, StatusCode> {
    let package_json_path = repo_path.join("package.json");
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

    let dockerfile_present = repo_path.join("Dockerfile").exists();
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
