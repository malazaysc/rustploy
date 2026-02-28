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
- Track internal server refactors in docs even when no API contract fields change.
- Return detected package manager and build profile in import responses.
- Return structured validation errors for invalid `rustploy.yaml` manifests.
- Support manual `force_rebuild` deploys (`POST /api/v1/apps/{app_id}/deployments`).

## Bootstrap behavior

- If there are no users and no API tokens yet, privileged endpoints are temporarily open.
- Once a bootstrap admin user exists (default) or any token exists, auth is required.
