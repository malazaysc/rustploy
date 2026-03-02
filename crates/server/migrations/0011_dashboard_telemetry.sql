CREATE TABLE IF NOT EXISTS agent_resource_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL,
    captured_at_unix_ms INTEGER NOT NULL,
    cpu_percent REAL NOT NULL,
    memory_used_bytes INTEGER NOT NULL,
    memory_total_bytes INTEGER NOT NULL,
    disk_used_bytes INTEGER NOT NULL,
    disk_total_bytes INTEGER NOT NULL,
    network_rx_bytes INTEGER NOT NULL,
    network_tx_bytes INTEGER NOT NULL,
    FOREIGN KEY(agent_id) REFERENCES agents(agent_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_agent_resource_samples_captured
    ON agent_resource_samples(captured_at_unix_ms DESC);

CREATE INDEX IF NOT EXISTS idx_agent_resource_samples_agent_captured
    ON agent_resource_samples(agent_id, captured_at_unix_ms DESC);

CREATE TABLE IF NOT EXISTS request_traffic_buckets (
    bucket_start_unix_ms INTEGER NOT NULL,
    app_id TEXT NOT NULL,
    total_requests INTEGER NOT NULL DEFAULT 0,
    errors_4xx INTEGER NOT NULL DEFAULT 0,
    errors_5xx INTEGER NOT NULL DEFAULT 0,
    updated_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (bucket_start_unix_ms, app_id)
);

CREATE INDEX IF NOT EXISTS idx_request_traffic_buckets_bucket
    ON request_traffic_buckets(bucket_start_unix_ms DESC);

CREATE INDEX IF NOT EXISTS idx_request_traffic_buckets_app_bucket
    ON request_traffic_buckets(app_id, bucket_start_unix_ms DESC);
