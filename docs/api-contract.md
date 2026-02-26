# API Contract (v0.1)

This document defines wire-level request/response shapes for Rustploy API endpoints used by Web UI, TUI, and external clients.
The canonical machine-readable source is [`../openapi.yaml`](../openapi.yaml).

Base path: `/api/v1`

## Common conventions

- `Content-Type: application/json`
- Authentication: `Authorization: Bearer <token>` for token clients
- Timestamps: RFC 3339 UTC strings
- IDs: UUID strings

## Error format

All non-2xx responses return:

```json
{
  "error": {
    "code": "invalid_request",
    "message": "Human readable summary",
    "details": {
      "field": "repository_url"
    },
    "request_id": "req_01J..."
  }
}
```

Error codes (initial):

- `invalid_request`
- `unauthorized`
- `forbidden`
- `not_found`
- `conflict`
- `rate_limited`
- `build_detection_failed`
- `internal_error`

## Import endpoint

`POST /apps/import`

Purpose: import a GitHub repository and detect build profile/package manager.

### Request

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
  "build_mode": "auto",
  "runtime": {
    "port": 3000,
    "healthcheck_path": "/"
  },
  "dependency_profile": {
    "postgres": false,
    "redis": false
  }
}
```

### Response (201)

```json
{
  "app": {
    "id": "6e7d3b70-b02d-4dc0-bd8e-f4b7e6e9d845",
    "name": "next-app",
    "created_at": "2026-02-26T18:20:00Z"
  },
  "detection": {
    "framework": "nextjs",
    "package_manager": "pnpm",
    "lockfile": "pnpm-lock.yaml",
    "build_profile": "nextjs-standalone-v1",
    "dockerfile_present": false
  },
  "next_action": {
    "type": "create_deployment",
    "deploy_endpoint": "/api/v1/apps/6e7d3b70-b02d-4dc0-bd8e-f4b7e6e9d845/deployments"
  }
}
```

## Deployment create endpoint

`POST /apps/{app_id}/deployments`

### Request

```json
{
  "source": {
    "type": "repo",
    "branch": "main",
    "commit_sha": "a1b2c3d4"
  },
  "build": {
    "mode": "auto",
    "profile": "nextjs-standalone-v1"
  },
  "reason": "manual"
}
```

### Response (202)

```json
{
  "deployment_id": "8fc14d1f-4f9b-4c24-9f35-2032a8b31f7f",
  "status": "queued",
  "queued_at": "2026-02-26T18:25:00Z"
}
```

## Pagination

List endpoints use cursor pagination:

- Request: `?limit=50&cursor=<opaque>`
- Response: `{ "items": [...], "next_cursor": "..." }`
