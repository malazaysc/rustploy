# Runbook: Single-Node Install

## Goal

Install Rustploy on one Linux host using Docker Compose.

## Preconditions

- Linux host with Docker and Docker Compose.
- Public DNS record pointing to host.
- Ports 80 and 443 reachable.
- Persistent disk path for data.

## Steps

1. Create install directory and copy compose files.
2. Set required environment variables (`RUSTPLOY_DOMAIN`, secrets, GitHub App values).
3. Start stack with `docker compose up -d`.
4. Confirm services are healthy.
5. Open web UI and complete first-admin setup.

## Validation

- API health endpoint returns success.
- TLS certificate is issued for configured domain.
- Agent heartbeat appears in system status.
