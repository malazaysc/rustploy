# Architecture

## High-level components

- `rustploy-server`: API, scheduler, state machine, and UI serving.
- `rustploy-agent`: host-side runtime executor and telemetry bridge.
- `rustploy-tui`: terminal operator client consuming `/api/v1`.
- `build subsystem`: repository detection and image build pipeline.
- `sqlite` (default): desired/actual state and durable job queue.
- `caddy` (default): ingress and TLS automation.

## Data model (initial)

- `apps`: metadata, owner, runtime config.
- `deployments`: image/tag/SHA, status, timestamps.
- `jobs`: durable command queue with retries/backoff.
- `domains`: hostname and TLS state.
- `github_integrations`: installation/repo mapping.
- `api_tokens`: token hash, scope, expiry, revocation state.
- `audit_events`: security and deployment event trail.

## Control flow

1. API updates desired state.
2. Reconciler compares desired vs actual state.
3. Reconciler writes jobs to queue.
4. Agent executes jobs via container runtime API.
5. Agent reports status back to server.

## Initial tech choices

- Rust: `axum`, `tokio`, `serde`, `sqlx`, `tracing`.
- Runtime API: Docker-compatible API first.
- UI: server-rendered Rust web UI with selective hydration.
- API contract: generated OpenAPI document for `/api/v1`.
- TUI: `ratatui` + `crossterm` client for SSH-based operations.
- Manifest: repository-level `rustploy.yaml` parser for deployment intent.
