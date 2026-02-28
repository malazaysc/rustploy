CREATE TABLE IF NOT EXISTS app_runtimes (
    app_id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL,
    project_name TEXT NOT NULL,
    service_name TEXT NOT NULL,
    upstream_host TEXT NOT NULL,
    upstream_port INTEGER NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(app_id) REFERENCES apps(id) ON DELETE CASCADE,
    FOREIGN KEY(deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_app_runtimes_deployment
    ON app_runtimes(deployment_id);
