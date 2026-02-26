# API Access

Rustploy should expose a stable API for automation, CI/CD, and custom internal tooling.
The API is also the shared contract used by both the web dashboard and the terminal UI.

Detailed wire-level schemas are defined in [`api-contract.md`](./api-contract.md).
The canonical OpenAPI source file is [`../openapi.yaml`](../openapi.yaml).

## API principles

- Versioned HTTP API under `/api/v1`.
- JSON request/response format.
- Idempotent deployment operations where possible.
- Auditability for token usage and deploy actions.

## Authentication model

- Session auth for web dashboard users.
- Personal access tokens (PAT) for user-level automation.
- Service tokens for machine-to-machine integration.
- Scoped permissions (read, deploy, admin).

## Minimum v0.1 endpoints

- `POST /api/v1/apps`
- `GET /api/v1/apps`
- `GET /api/v1/apps/{app_id}`
- `PATCH /api/v1/apps/{app_id}`
- `POST /api/v1/apps/import` (repo import + build profile detection)
- `POST /api/v1/apps/{app_id}/deployments`
- `GET /api/v1/apps/{app_id}/deployments`
- `POST /api/v1/apps/{app_id}/rollback`
- `POST /api/v1/apps/{app_id}/domains`
- `GET /api/v1/apps/{app_id}/logs/stream` (SSE)
- `POST /api/v1/tokens`
- `GET /api/v1/tokens`
- `DELETE /api/v1/tokens/{token_id}`

## Security requirements

- Token hashes stored, never raw tokens.
- Expiration and revocation support.
- Rate limits per token and per IP.
- Full webhook signature verification.

## Developer experience

- Publish OpenAPI spec from source.
- Generate API docs page from OpenAPI.
- Include curl examples in user docs.
- Keep API compatibility stable across web and TUI client releases.
- Return detected package manager and build profile in import responses.
