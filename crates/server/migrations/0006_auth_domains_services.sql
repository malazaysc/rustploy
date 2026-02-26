CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    session_token_hash TEXT NOT NULL UNIQUE,
    expires_at_unix_ms INTEGER NOT NULL,
    revoked_at_unix_ms INTEGER,
    created_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_sessions_user_expires
    ON sessions(user_id, expires_at_unix_ms DESC);

CREATE TABLE IF NOT EXISTS password_reset_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at_unix_ms INTEGER NOT NULL,
    used_at_unix_ms INTEGER,
    created_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_password_reset_tokens_user
    ON password_reset_tokens(user_id, created_at_unix_ms DESC);

CREATE TABLE IF NOT EXISTS domains (
    id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    domain TEXT NOT NULL UNIQUE,
    tls_mode TEXT NOT NULL DEFAULT 'managed',
    cert_path TEXT,
    key_path TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(app_id) REFERENCES apps(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_domains_app
    ON domains(app_id, created_at_unix_ms DESC);

CREATE TABLE IF NOT EXISTS managed_services (
    id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    service_type TEXT NOT NULL,
    username TEXT NOT NULL,
    password TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    healthy INTEGER NOT NULL DEFAULT 1,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    UNIQUE(app_id, service_type),
    FOREIGN KEY(app_id) REFERENCES apps(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_managed_services_app
    ON managed_services(app_id, service_type);
