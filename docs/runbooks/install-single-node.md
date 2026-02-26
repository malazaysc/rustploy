# Runbook: Single-Node Install

## Goal

Install Rustploy on one Linux host using Docker Compose.

## Preconditions

- Linux host with Docker and Docker Compose.
- Public DNS record pointing to host.
- Ports 80 and 443 reachable.
- Persistent disk path for data.

## Steps

1. Create install directory and copy project files.
2. Set bootstrap credentials and optional secrets in `docker-compose.yml` (or an env file):
   - `RUSTPLOY_ADMIN_EMAIL`
   - `RUSTPLOY_ADMIN_PASSWORD`
   - `RUSTPLOY_GITHUB_WEBHOOK_SECRET` (optional)
   - `RUSTPLOY_AGENT_TOKEN` (optional)
3. Start stack:
   - `docker compose up -d --build`
4. Verify API and auth:
   - `curl http://localhost:8080/api/v1/health`
   - `curl -sS -c /tmp/rustploy.cookies -X POST http://localhost:8080/api/v1/auth/login -H 'content-type: application/json' -d '{"email":"admin@localhost","password":"admin"}'`
5. Open dashboard at `http://<host>:8080/` and import first app.

## Validation

- API health endpoint returns success.
- Domain mapping API works (`POST /api/v1/apps/{app_id}/domains`).
- Caddy certificate state persists in compose volumes after restart.
- Agent heartbeat appears in system status.
