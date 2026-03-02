# API Access

Rustploy should expose a stable API for automation, CI/CD, and custom internal tooling.
The API is also the shared contract used by both the web dashboard and the terminal UI.

Detailed wire-level schemas are defined in [`api-contract.md`](./api-contract.md).
The canonical OpenAPI source file is [`../openapi.yaml`](../openapi.yaml).

## API principles

- Versioned HTTP API under `/api/v1` (with `/metrics` exposed at root).
- JSON request/response format.
- Idempotent deployment operations where possible.
- Auditability for token usage and deploy actions.

## Authentication model

- Session auth for web dashboard users.
- Personal access tokens (PAT) for user-level automation.
- Service tokens for machine-to-machine integration.
- Scoped permissions (read, deploy, admin).

## Minimum v0.1 endpoints

- `GET /api/v1/health`
- `GET /metrics`
- `POST /api/v1/auth/login`
- `POST /api/v1/auth/logout`
- `GET /api/v1/auth/me`
- `POST /api/v1/auth/password-reset/request`
- `POST /api/v1/auth/password-reset/confirm`
- `GET /api/v1/dashboard/metrics`
- `POST /api/v1/apps/import`
- `POST /api/v1/apps`
- `GET /api/v1/apps`
- `POST /api/v1/apps/{app_id}/github`
- `GET /api/v1/apps/{app_id}/config`
- `GET /api/v1/apps/{app_id}/env`
- `PUT /api/v1/apps/{app_id}/env`
- `DELETE /api/v1/apps/{app_id}/env/{key}`
- `POST /api/v1/apps/{app_id}/domains`
- `GET /api/v1/apps/{app_id}/domains`
- `POST /api/v1/apps/{app_id}/deployments`
- `GET /api/v1/apps/{app_id}/deployments`
- `GET /api/v1/apps/{app_id}/deployments/{deployment_id}/logs`
- `GET /api/v1/apps/{app_id}/logs/stream`
- `POST /api/v1/apps/{app_id}/rollback`
- `POST /api/v1/tokens`
- `GET /api/v1/tokens`
- `DELETE /api/v1/tokens/{token_id}`
- `POST /api/v1/integrations/github/webhook`

## Security requirements

- Token hashes stored, never raw tokens.
- Expiration and revocation support.
- Token scope enforcement (`read`, `deploy`, `admin`).
- Full webhook signature verification.

## Developer experience

- Publish OpenAPI spec from source.
- Include curl examples in user docs.
- Keep API compatibility stable across web and TUI client releases.
- The `/logs` web explorer UI consumes existing deployment and logs endpoints (`GET /api/v1/apps`, `GET /api/v1/apps/{app_id}/deployments`, `GET /api/v1/apps/{app_id}/deployments/{deployment_id}/logs`) and does not introduce a new wire contract.
- Logs explorer query execution aggregates successful per-app deployment fetches and surfaces partial-failure warnings when one app query fails.
- Logs explorer query UI applies a latest-request guard so stale out-of-order responses are dropped instead of overwriting newer query results.
- Dashboard domain and env list rendering now uses text-safe DOM node assembly, preserving endpoint contracts while preventing stored script injection from user-controlled fields.
- Dashboard live-log client now tears down app-level SSE streams when app selection is cleared, preventing stale background requests against `/api/v1/apps/{app_id}/logs/stream`.
- Dashboard metrics endpoint (`GET /api/v1/dashboard/metrics`) returns live summary cards plus request traffic/resource time series for configurable windows (`1h|24h|7d`) and buckets (`1m|5m|1h`), optionally filtered by `app_id` for request traffic.
- Agent heartbeat payloads may also include optional host resource fields (`cpu_percent`, memory/disk bytes, network RX/TX bytes), and older heartbeats without `resource` are still accepted.
- Agent heartbeat `timestamp_unix_ms` is client-reported metadata; server persistence clamps it to receive time to prevent skewed telemetry samples.
- Dashboard metrics client refreshes apply latest-request guards so stale responses are dropped when app scope changes mid-request.
- Track internal server refactors in docs even when no API contract fields change.
- Stream endpoint pushes incremental deployment log chunks whenever new lines arrive from compose/git runtime commands.
- Stream `logs` events use JSON payloads (`deployment_id`, `logs`, `reset`) over SSE so clients can safely handle escaped content and reconnect snapshots.
- Streamed runtime command logs are UTF-8 tolerant (lossy decode) and apply redaction before surfaced failure text.
- Stream payload encoding escapes both `\\n` and `\\r` via JSON encoding so SSE transport remains valid for multiline runtime logs.
- Incremental log stream cursor reads are backed by a composite SQLite index on deployment log rows for lower polling cost under concurrent viewers.
- Command execution failures now return concise errors while full command output remains in deployment logs.
- Return detected package manager and build profile in import responses.
- Return structured validation errors for invalid `rustploy.yaml` manifests.
- Support manual `force_rebuild` deploys (`POST /api/v1/apps/{app_id}/deployments`).

## Bootstrap behavior

- If there are no users and no API tokens yet, privileged endpoints are temporarily open.
- Once a bootstrap admin user exists (default) or any token exists, auth is required.
