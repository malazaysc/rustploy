# Caddy Ingress and Certificates

Rustploy can generate a Caddyfile from app domain mappings.

## Behavior

- Domain mappings are stored in SQLite (`domains` table).
- On startup and domain changes, Rustploy writes Caddy config to `RUSTPLOY_CADDYFILE_PATH`.
- Reverse proxy target is configurable via `RUSTPLOY_UPSTREAM_ADDR` (defaults to `127.0.0.1:8080`).
- Access logs for app-facing routes are emitted in JSON format to `RUSTPLOY_CADDY_ACCESS_LOG_PATH` (defaults to `/shared/caddy-access.log`) for dashboard traffic telemetry ingestion.
- Access-log ingestion can be toggled with `RUSTPLOY_CADDY_ACCESS_LOG_ENABLED` (disabled by default unless a custom log path is provided).
- `tls_mode=managed` uses automatic ACME certificates.
- `tls_mode=custom` uses explicit `cert_path` and `key_path`.

## Docker Compose

The default `docker-compose.yml` includes a `caddy` service with persistent `rustploy-caddy` and `rustploy-caddy-config` volumes so certificate state survives restarts/upgrades. Caddyfile exchange stays on a read-only shared volume, while access logs are written to a dedicated `rustploy-caddy-logs` volume mounted at `/var/log/caddy` and read by `rustploy-server`.

## API

- `POST /api/v1/apps/{app_id}/domains`
- `GET /api/v1/apps/{app_id}/domains`
