# Architecture

## High-level components

- `rustploy-server`: API, reconciler, deployment executor, and embedded dashboard UI.
- `rustploy-agent`: lightweight heartbeat/agent telemetry process.
- `rustploy-tui`: terminal operator client consuming `/api/v1`.
- `sqlite` (default): durable state, queue, auth, routing, and deployment history.
- `caddy` (default): ingress/router and TLS termination.
- `docker compose` runtime: per-app compose projects started by `rustploy-server` via Docker socket.

## Data model (current)

- `apps`: app metadata.
- `app_import_configs`: repo/source/build detection and compose summary.
- `app_env_vars`: per-app runtime environment variables.
- `deployments`: deployment requests and state.
- `deployment_logs`: deployment event/log lines.
- `jobs`: durable reconciler queue with retry/backoff.
- `app_runtimes`: active runtime route target per app.
- `domains`: domain and TLS mode mappings.
- `managed_services`: generated credentials/endpoints for managed dependency placeholders.
- `github_integrations`: owner/repo/branch mapping for webhook-triggered deploys.
- `api_tokens` + `token_audit_events`: machine auth and API audit trail.
- `users`, `sessions`, `password_reset_tokens`: dashboard auth/session lifecycle.
- `agents`, `agent_heartbeats`: agent registration and liveness.

## Control flow

1. API request creates/updates desired state (for example import, env var update, deploy request).
2. Deploy request inserts deployment row + queued job.
3. Reconciler claims due jobs and executes deployment in-process.
4. For compose deploys, server clones repo, prepares runtime compose, injects app env vars, and runs Docker Compose.
5. Server records logs/status, updates app runtime route, and re-syncs Caddy routing.

## Tech stack

- Rust: `axum`, `tokio`, `serde`, `rusqlite`, `tracing`.
- Runtime integration: Docker CLI + Compose plugin (`docker compose`) through mounted Docker socket.
- UI: server-rendered HTML/CSS/JS dashboard served by `rustploy-server`.
- API contract: OpenAPI source in `openapi.yaml` (versioned HTTP API under `/api/v1`).
- TUI: `ratatui` + `crossterm` for SSH-friendly operations.
- Manifest: lightweight `rustploy.yaml` parser for import-time build/dependency hints.
