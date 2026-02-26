# Runbook: Single-Node Upgrade

## Goal

Upgrade Rustploy with minimal downtime and no state loss.

## Preconditions

- Existing healthy installation.
- Recent backup of SQLite DB and Caddy data.
- Release notes reviewed for breaking changes.

## Steps

1. Pull new container images or binaries.
2. Stop write-heavy operations (optional maintenance window).
3. Apply DB migrations.
4. Restart services in order: server, agent, ingress.
5. Verify health and recent deployments.

## Validation

- API and UI are reachable.
- Existing apps are listed and healthy.
- New deployments can be triggered successfully.
