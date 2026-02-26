CREATE TABLE IF NOT EXISTS deployment_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    deployment_id TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    FOREIGN KEY(deployment_id) REFERENCES deployments(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_deployment_logs_deployment_time
    ON deployment_logs(deployment_id, created_at_unix_ms ASC);
