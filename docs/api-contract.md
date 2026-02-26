# API Contract (v0.1)

This document summarizes the currently implemented wire contract.
The canonical machine-readable source is [`../openapi.yaml`](../openapi.yaml).

Base path: `/api/v1`

## Common conventions

- `Content-Type: application/json`
- Token auth: `Authorization: Bearer <token>`
- Agent auth (optional): `x-rustploy-agent-token: <token>`
- IDs: UUID strings
- Timestamps: unix milliseconds

## Bootstrap behavior

- If no API token exists yet, privileged endpoints are temporarily open.
- After the first token is created, token auth is required.

## Implemented endpoint groups

- Health: `GET /health`
- Agents: `GET /agents`, `POST /agents/register`, `POST /agents/heartbeat`
- Apps/import: `POST /apps/import`, `GET /apps`, `POST /apps`
- GitHub: `POST /apps/{app_id}/github`, `POST /integrations/github/webhook`
- Deployments: `GET/POST /apps/{app_id}/deployments`, `POST /apps/{app_id}/rollback`
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
