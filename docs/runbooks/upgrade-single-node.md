# Runbook: Single-Node Upgrade

## Goal

Upgrade Rustploy with minimal downtime and no state loss.

## Preconditions

- Existing healthy installation.
- Recent backup of SQLite DB and Caddy data.
- Release notes reviewed for breaking changes.

## Steps

1. Take backup using [`backup-and-restore.md`](./backup-and-restore.md).
2. Pull latest code/images.
3. Restart stack with rebuild:
   - `docker compose up -d --build`
4. Confirm migrations completed in server logs.
5. Verify health and recent deployments.

## Validation

- API and UI are reachable.
- Existing apps are listed and healthy.
- New deployments can be triggered successfully.
- Existing domain/TLS configuration still works after restart.
