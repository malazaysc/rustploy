use std::{
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
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shared::{
    AgentHeartbeat, AgentListResponse, AgentRegisterRequest, AgentRegistered, AgentStatus,
    AgentSummary, AppListResponse, AppSummary, CreateAppRequest, CreateDeploymentAccepted,
    CreateDeploymentRequest, CreateTokenRequest, CreateTokenResponse, DependencyProfile,
    DeploymentListResponse, DeploymentStatus, DeploymentSummary, DetectionResult,
    GithubConnectRequest, GithubIntegrationSummary, GithubWebhookAccepted, HealthResponse,
    HeartbeatAccepted, ImportAppRequest, ImportAppResponse, NextAction, RepositoryRef, SourceRef,
    TokenListResponse, TokenSummary,
};
use tracing::{error, warn};
use uuid::Uuid;

const DEFAULT_ONLINE_WINDOW_MS: u64 = 30_000;
const DEFAULT_DB_PATH: &str = "data/rustploy.db";
const DEFAULT_RECONCILER_ENABLED: bool = true;
const DEFAULT_RECONCILER_INTERVAL_MS: u64 = 1_000;
const DEFAULT_JOB_BACKOFF_BASE_MS: u64 = 1_000;
const DEFAULT_JOB_BACKOFF_MAX_MS: u64 = 30_000;
const DEFAULT_JOB_MAX_ATTEMPTS: u32 = 5;
const AGENT_TOKEN_HEADER: &str = "x-rustploy-agent-token";
const GITHUB_SIGNATURE_HEADER: &str = "x-hub-signature-256";
const GITHUB_EVENT_HEADER: &str = "x-github-event";
const AUTHORIZATION_HEADER: &str = "authorization";

const SCOPE_READ: u8 = 1;
const SCOPE_DEPLOY: u8 = 1 << 1;
const SCOPE_ADMIN: u8 = 1 << 2;

#[derive(Clone, Debug)]
pub struct AppState {
    db: Database,
    agent_shared_token: Option<String>,
    github_webhook_secret: Option<String>,
    online_window_ms: u64,
    reconciler_enabled: bool,
    reconciler_interval_ms: u64,
    job_backoff_base_ms: u64,
    job_backoff_max_ms: u64,
    job_max_attempts: u32,
}

#[derive(Clone, Debug)]
struct AppStateConfig {
    db_path: String,
    agent_shared_token: Option<String>,
    github_webhook_secret: Option<String>,
    online_window_ms: u64,
    reconciler_enabled: bool,
    reconciler_interval_ms: u64,
    job_backoff_base_ms: u64,
    job_backoff_max_ms: u64,
    job_max_attempts: u32,
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
        })
    }

    fn from_config(config: AppStateConfig) -> Result<Self> {
        let db = Database::open(&config.db_path)
            .with_context(|| format!("failed to open db at {}", config.db_path))?;

        Ok(Self {
            db,
            agent_shared_token: config.agent_shared_token,
            github_webhook_secret: config.github_webhook_secret,
            online_window_ms: config.online_window_ms,
            reconciler_enabled: config.reconciler_enabled,
            reconciler_interval_ms: config.reconciler_interval_ms,
            job_backoff_base_ms: config.job_backoff_base_ms,
            job_backoff_max_ms: config.job_backoff_max_ms,
            job_max_attempts: config.job_max_attempts,
        })
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

    #[cfg(test)]
    fn for_tests(db_path: &str, agent_shared_token: Option<String>) -> Self {
        Self::from_config(AppStateConfig {
            db_path: db_path.to_string(),
            agent_shared_token,
            github_webhook_secret: None,
            online_window_ms: DEFAULT_ONLINE_WINDOW_MS,
            reconciler_enabled: true,
            reconciler_interval_ms: 1,
            job_backoff_base_ms: 1,
            job_backoff_max_ms: 100,
            job_max_attempts: DEFAULT_JOB_MAX_ATTEMPTS,
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
                status,
                simulate_failures,
                created_at_unix_ms,
                updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                deployment_id.to_string(),
                app_id.to_string(),
                request.source_ref,
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
                "SELECT id, app_id, source_ref, status, last_error, created_at_unix_ms, updated_at_unix_ms
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
            let status_raw: String = row.get(3).context("failed reading deployment status")?;
            let last_error: Option<String> = row.get(4).context("failed reading last error")?;
            let created_raw: i64 = row.get(5).context("failed reading created time")?;
            let updated_raw: i64 = row.get(6).context("failed reading updated time")?;

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
                status,
                last_error,
                created_at_unix_ms: to_u64(created_raw)?,
                updated_at_unix_ms: to_u64(updated_raw)?,
            });
        }

        Ok(items)
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

        if job.attempt_count <= payload.simulate_failures {
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
            self.set_deployment_state(
                payload.deployment_id,
                DeploymentStatus::Retrying,
                Some(error_message),
                now_unix_ms,
            )?;
        } else {
            self.mark_job_failed(job.id, now_unix_ms, error_message)?;
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

    Ok(())
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
        .route("/api/v1/health", get(health))
        .route("/api/v1/apps/import", post(import_app))
        .route("/api/v1/apps", get(list_apps).post(create_app))
        .route("/api/v1/apps/:app_id/github", post(connect_github_repo))
        .route(
            "/api/v1/apps/:app_id/deployments",
            get(list_app_deployments).post(create_deployment),
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

async fn import_app(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ImportAppRequest>,
) -> Result<(StatusCode, Json<ImportAppResponse>), StatusCode> {
    authorize_api_request(
        &state,
        &headers,
        RequiredScope::Admin,
        "POST",
        "/api/v1/apps/import",
    )?;

    let owner = payload.repository.owner.trim().to_string();
    let repo = payload.repository.name.trim().to_string();
    if owner.is_empty() || repo.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
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
    clone_repository(&clone_url, &branch, &temp_path)?;
    let import_result = (|| -> Result<ImportAppResponse, StatusCode> {
        let manifest = read_manifest(&temp_path).map_err(|_| StatusCode::BAD_REQUEST)?;
        let manifest_json = manifest
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let detection = detect_build_profile(&temp_path).map_err(|_| StatusCode::BAD_REQUEST)?;
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
            return Err(StatusCode::BAD_REQUEST);
        }

        let app_name = manifest
            .as_ref()
            .and_then(|value| value.app.as_ref()?.name.clone())
            .unwrap_or_else(|| repo.clone());

        let now = now_unix_ms();
        let app = state
            .db
            .create_app(&app_name, now)
            .map_err(internal_error)?
            .ok_or(StatusCode::CONFLICT)?;

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
            .map_err(internal_error)?;
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
            .map_err(internal_error)?;

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

    import_result.map(|payload| (StatusCode::CREATED, Json(payload)))
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

fn authorize_api_request(
    state: &AppState,
    headers: &HeaderMap,
    required_scope: RequiredScope,
    method: &str,
    path: &str,
) -> Result<(), StatusCode> {
    let has_tokens = state.db.has_unrevoked_tokens().map_err(internal_error)?;
    if !has_tokens {
        return Ok(());
    }

    let token = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let now = now_unix_ms();
    let maybe_token = state
        .db
        .authenticate_api_token(&token_hash(token), now, method, path)
        .map_err(internal_error)?;
    let authenticated = maybe_token.ok_or(StatusCode::UNAUTHORIZED)?;
    if scope_allows(authenticated.scope_mask, required_scope) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
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

fn read_manifest(repo_path: &Path) -> Result<Option<RustployManifest>, StatusCode> {
    let manifest_path = repo_path.join("rustploy.yaml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&manifest_path).map_err(|_| StatusCode::BAD_REQUEST)?;
    let manifest = parse_manifest_yaml(&content).map_err(|_| StatusCode::BAD_REQUEST)?;
    if manifest.version.unwrap_or(1) != 1 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Some(manifest))
}

fn parse_manifest_yaml(content: &str) -> Result<RustployManifest> {
    let mut manifest = RustployManifest {
        version: None,
        app: None,
        build: None,
        dependencies: None,
    };

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
                    manifest.version =
                        Some(value.parse::<u32>().context("invalid manifest version")?);
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
                        let enabled = parse_yaml_bool(value)?;
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

    Ok(manifest)
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
    error!(%error, "request failed");
    StatusCode::INTERNAL_SERVER_ERROR
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
            online_window_ms: DEFAULT_ONLINE_WINDOW_MS,
            reconciler_enabled: false,
            reconciler_interval_ms: 1,
            job_backoff_base_ms: 1,
            job_backoff_max_ms: 100,
            job_max_attempts: DEFAULT_JOB_MAX_ATTEMPTS,
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
