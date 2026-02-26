CREATE TABLE IF NOT EXISTS github_integrations (
    id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    branch TEXT NOT NULL,
    installation_id INTEGER,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    UNIQUE(app_id),
    UNIQUE(owner, repo, branch, app_id),
    FOREIGN KEY(app_id) REFERENCES apps(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_github_integrations_repo_branch
    ON github_integrations(owner, repo, branch);
