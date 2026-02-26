CREATE TABLE IF NOT EXISTS agents (
    agent_id TEXT PRIMARY KEY,
    agent_version TEXT NOT NULL,
    first_seen_unix_ms INTEGER NOT NULL,
    last_seen_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_heartbeats (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL,
    reported_unix_ms INTEGER NOT NULL,
    received_unix_ms INTEGER NOT NULL,
    agent_version TEXT NOT NULL,
    FOREIGN KEY(agent_id) REFERENCES agents(agent_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_agent_heartbeats_agent_id_received
    ON agent_heartbeats(agent_id, received_unix_ms DESC);
