# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and this project aims to follow Semantic Versioning.

## [Unreleased]

### Added

- Documentation-first project structure.
- Core architecture, roadmap, and task backlog.
- Initial API access planning document.
- Terminal UI strategy document and backlog task.
- Next.js zero-config build strategy and package manager support planning.
- API contract, manifest schema, and deployment state machine specs.
- Initial ADR set, OpenAPI source file, example manifests, and runbooks.
- Initial Rust workspace scaffold (`server`, `agent`, `tui`, `shared`).
- Basic API health and agent heartbeat endpoints with tests.
- CI workflow for fmt, clippy, and tests.
- SQLite-backed agent registration and heartbeat tracking (`/api/v1/agents`).
- Stub API routes for apps and deployments (`/api/v1/apps`, `/api/v1/apps/:id/deployments`).
- Containerized runtime via `Dockerfile` and `docker-compose.yml`.
- Durable SQLite job queue and deployment reconciler loop with retry/backoff.
- App and deployment CRUD baseline (`POST/GET /api/v1/apps`, `POST/GET /api/v1/apps/:id/deployments`).
- GitHub repo mapping + webhook verification with deployment enqueue (`/api/v1/apps/:id/github`, `/api/v1/integrations/github/webhook`).
- API token lifecycle and scope enforcement (`/api/v1/tokens`, revoke, audit trail, bearer auth).
- App import endpoint with Git-based Next.js/package-manager detection and `rustploy.yaml` parsing (`/api/v1/apps/import`).
- Rollback endpoint using latest healthy source (`/api/v1/apps/:id/rollback`).
- Expanded `rustploy-tui` command mode for listing apps/deployments and triggering deploy/rollback with API tokens.
