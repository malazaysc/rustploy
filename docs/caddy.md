# Caddy Ingress and Certificates

Rustploy can generate a Caddyfile from app domain mappings.

## Behavior

- Domain mappings are stored in SQLite (`domains` table).
- On startup and domain changes, Rustploy writes Caddy config to `RUSTPLOY_CADDYFILE_PATH`.
- Reverse proxy target is configurable via `RUSTPLOY_UPSTREAM_ADDR` (defaults to `127.0.0.1:8080`).
- `tls_mode=managed` uses automatic ACME certificates.
- `tls_mode=custom` uses explicit `cert_path` and `key_path`.

## Docker Compose

The default `docker-compose.yml` includes a `caddy` service with persistent `rustploy-caddy` and `rustploy-caddy-config` volumes so certificate state survives restarts/upgrades.

## API

- `POST /api/v1/apps/{app_id}/domains`
- `GET /api/v1/apps/{app_id}/domains`
