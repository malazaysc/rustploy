CREATE TABLE IF NOT EXISTS app_import_configs (
    app_id TEXT PRIMARY KEY,
    repo_provider TEXT NOT NULL,
    repo_owner TEXT NOT NULL,
    repo_name TEXT NOT NULL,
    repo_branch TEXT NOT NULL,
    build_mode TEXT NOT NULL,
    package_manager TEXT NOT NULL,
    framework TEXT NOT NULL,
    lockfile TEXT,
    build_profile TEXT NOT NULL,
    dockerfile_present INTEGER NOT NULL DEFAULT 0,
    dependency_profile_json TEXT,
    manifest_json TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(app_id) REFERENCES apps(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_app_import_configs_repo
    ON app_import_configs(repo_owner, repo_name, repo_branch);

CREATE TABLE IF NOT EXISTS api_tokens (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    scope_mask INTEGER NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER,
    revoked_at_unix_ms INTEGER,
    last_used_at_unix_ms INTEGER
);

CREATE INDEX IF NOT EXISTS idx_api_tokens_hash
    ON api_tokens(token_hash);

CREATE TABLE IF NOT EXISTS token_audit_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id TEXT NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(token_id) REFERENCES api_tokens(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_token_audit_events_token_time
    ON token_audit_events(token_id, created_at_unix_ms DESC);
