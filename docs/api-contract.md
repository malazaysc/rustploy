# API Contract (v0.1)

This document summarizes the currently implemented wire contract.
The canonical machine-readable source is [`../openapi.yaml`](../openapi.yaml).

Base path: `/api/v1` (metrics endpoint is additionally exposed at `/metrics`).

## Common conventions

- `Content-Type: application/json`
- Token auth: `Authorization: Bearer <token>`
- Session auth: cookie `rustploy_session=<token>`
- Agent auth (optional): `x-rustploy-agent-token: <token>`
- IDs: UUID strings
- Timestamps: unix milliseconds

## Bootstrap behavior

- If no users and no API tokens exist, privileged endpoints are temporarily open.
- Once a user or token exists, bearer token or session auth is required.

## Implemented endpoint groups

- Health/metrics: `GET /health` (also exposed at `/api/v1/health`), `GET /metrics`
- Auth: `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`, password reset endpoints
- Agents: `GET /agents`, `POST /agents/register`, `POST /agents/heartbeat`
- Dashboard telemetry: `GET /dashboard/metrics`
- Apps/import: `POST /apps/import`, `GET /apps`, `POST /apps`
- App runtime probe: `GET /apps/{app_id}/runtime`
- App container inventory:
  - `GET /apps/{app_id}/containers`
  - `GET /apps/{app_id}/containers/{container_id}`
  - `GET /apps/{app_id}/containers/{container_id}/logs`
  - `GET /apps/{app_id}/containers/{container_id}/logs/stream`
  - `GET /apps/{app_id}/containers/{container_id}/logs/download`
- Effective config: `GET /apps/{app_id}/config`
- Env vars: `GET/PUT /apps/{app_id}/env`, `DELETE /apps/{app_id}/env/{key}`
- Domains: `GET/POST /apps/{app_id}/domains`
- GitHub: `POST /apps/{app_id}/github`, `POST /integrations/github/webhook`
- Deployments: `GET/POST /apps/{app_id}/deployments`, `GET /apps/{app_id}/deployments/{deployment_id}/logs`, `GET /apps/{app_id}/logs/stream`, `POST /apps/{app_id}/rollback`
  - `POST /apps/{app_id}/deployments` supports optional `force_rebuild: true` for no-cache compose rebuilds.
- Tokens: `GET/POST /tokens`, `DELETE /tokens/{token_id}`

## Example: create admin token

```json
{
  "name": "admin",
  "scopes": ["admin"],
  "expires_in_seconds": null
}
```

Response (201):

```json
{
  "token": "rp_<redacted>",
  "summary": {
    "id": "f3f8c0ed-c2d1-46c6-9a24-910398f7afc0",
    "name": "admin",
    "scopes": ["admin", "deploy", "read"],
    "created_at_unix_ms": 1708980000000,
    "expires_at_unix_ms": null,
    "revoked_at_unix_ms": null,
    "last_used_at_unix_ms": null
  }
}
```

## Example: import app

```json
{
  "repository": {
    "provider": "github",
    "owner": "acme",
    "name": "next-app",
    "default_branch": "main"
  },
  "source": {
    "branch": "main",
    "commit_sha": null
  },
  "build_mode": "auto"
}
```

Response includes detected framework/package manager and deploy endpoint for the imported app.

If `rustploy.yaml` is invalid, response is `400` with structured fields under:

- `error.code` (e.g. `invalid_manifest`)
- `error.message`
- `error.details[]` (`field`, `message`)

## SSE logs stream payload

`GET /apps/{app_id}/logs/stream` emits `event: logs` events with JSON payloads:

```json
{
  "deployment_id": "uuid-or-null",
  "logs": "new log chunk text",
  "reset": false
}
```

- `deployment_id` changes when a newer deployment becomes the active stream source.
- `logs` contains only newly appended lines for the active deployment (first event after connection/deployment change can include accumulated lines).
- `reset` is `true` when clients should replace displayed content (initial snapshot/reconnect/deployment switch).
- When a deployment switch has no log lines yet, the stream emits a single `reset: true` event with empty `logs` so clients can clear stale output.

## Agent heartbeat resource payload

`POST /agents/heartbeat` accepts an optional `resource` object:

- `cpu_percent`
- `memory_used_bytes`, `memory_total_bytes`
- `disk_used_bytes`, `disk_total_bytes`
- `network_rx_bytes`, `network_tx_bytes`

When omitted, heartbeat behavior remains unchanged.

## Dashboard metrics endpoint

`GET /dashboard/metrics` supports query parameters:

- `window`: `1h` | `24h` | `7d` (default `24h`)
- `bucket`: `1m` | `5m` | `1h` (default depends on window)
- `app_id`: optional UUID for app-scoped request traffic

Response includes:

- `summary` (applications, healthy managed services, domains, uptime proxy)
- `scope` (effective window/bucket/app scope and unix-ms range)
- `request_traffic[]` (bucketed totals + 4xx/5xx)
- `server_resources[]` (bucketed host resource averages + sample counts)

## App runtime health endpoint

`GET /apps/{app_id}/runtime` returns the current runtime probe state for an app:

- `configured`: whether an `app_runtimes` route exists for the app
- `reachable`: TCP probe result to current runtime upstream
- `upstream_host`, `upstream_port`: current runtime endpoint (nullable when unconfigured)
- `deployment_id`: deployment currently bound to runtime route (nullable when unconfigured)
- `checked_at_unix_ms`: probe timestamp

## App container inventory endpoints

`GET /apps/{app_id}/containers` returns project-scoped runtime container summaries:

- `project_name`: compose project identifier used for container discovery
- `items[]` per container:
  - `id`, `name`, optional `service_name`
  - `status` (`running`, `restarting`, `paused`, `exited`, `dead`, `unhealthy`, `unknown`)
  - `started_at`, `restart_count`, `image`
  - `port_mappings[]` (`container_port`, `protocol`, optional `host_ip`, optional `host_port`)
  - optional `health` (`status`, `last_output`)

`GET /apps/{app_id}/containers/{container_id}` returns detailed metadata for one runtime container:

- Summary fields above plus:
  - `created_at`, `command[]`, `labels{}` map
  - `exposed_ports[]`
  - `networks[]` (`network_name`, optional `ip_address`, `gateway`, `mac_address`)
  - `mounts[]` (`mount_type`, optional `source`, `destination`, `mode`, `read_only`)
  - `env[]` entries include `masked` flag; sensitive values are always redacted
- If `container_id` is an ambiguous prefix that matches multiple containers, response is `409 Conflict`.

## App container logs endpoints

`GET /apps/{app_id}/containers/{container_id}/logs` returns a filtered container log chunk:

- Query parameters:
  - `since` (optional unix ms cursor)
  - `until` (optional unix ms upper bound)
  - `tail` (optional line count, defaults to `2000` when `since` is omitted; capped at `10000`)
  - `contains` (optional case-insensitive substring filter)
- Response includes:
  - `logs` (newline-delimited text)
  - `next_since_unix_ms` cursor for reconnect/incremental follow-up requests
- Invalid ranges (`since > until`) return `400 Bad Request`.
- Missing runtime/container resolution returns `404 Not Found`.
- Ambiguous container selector matches return `409 Conflict`.

`GET /apps/{app_id}/containers/{container_id}/logs/stream` emits `event: logs` SSE frames with JSON payload:

```json
{
  "container_id": "3ef8fd7f5356",
  "logs": "2026-03-03T10:00:00.123456789Z service ready",
  "reset": false,
  "next_since_unix_ms": 1772532000124
}
```

- Stream supports `since`, `until`, `tail`, and `contains` query parameters.
- `tail` defaults to `200` on initial stream requests when `since` is omitted.
- `reset` is `true` on initial stream snapshots so clients can replace stale output.
- Reconnect clients should pass `next_since_unix_ms` back as `since` to continue without major gaps.
- Missing runtime/container resolution returns `404 Not Found`.
- Ambiguous container selector matches return `409 Conflict`.

`GET /apps/{app_id}/containers/{container_id}/logs/download` returns plain-text logs as an attachment and supports the same filter/range query parameters.
- Missing runtime/container resolution returns `404 Not Found`.
- Ambiguous container selector matches return `409 Conflict`.
