# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and this project aims to follow Semantic Versioning.

## [Unreleased]

### Changed

- Internal clippy-driven cleanup in `crates/server/src/lib.rs` (removed needless borrows); no user-visible behavior change.
- Improved live deployment log robustness and error handling, including tolerant decoding for non-UTF8 runtime output.
- Improved live log stream efficiency by switching SSE updates to incremental log chunks per deployment.
- Dashboard deployment UX now shows optimistic `queued/building` status immediately after deploy actions and keeps status pills updated during rollout.
- Dashboard SSE client now consumes structured JSON log events to preserve literal escaped sequences safely.

### Fixed

- SSE log payload encoding now escapes carriage returns as well as newlines to prevent stream panics on runtime logs.
- Dashboard live logs no longer retain stale content when active deployment changes before new log lines are written.
- Manual deployment log view now synchronizes stream deployment selection to avoid mixed incremental output.

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
- Structured manifest validation errors for import requests.
- Effective app config endpoint (`GET /api/v1/apps/:id/config`) and TUI config inspection command.
- Deployment logs persistence and logs endpoint (`GET /api/v1/apps/:id/deployments/:deployment_id/logs`) with TUI `logs` command.
- Compose runtime deployment execution against real Docker Compose projects (clone/build/up/health checks) with runtime routing persistence.
- App environment variable management (`GET/PUT/DELETE /api/v1/apps/:id/env`) with dashboard controls and runtime injection into app service environment.
- Manual deployment resync/rebuild support via `force_rebuild` deployment option and dashboard "Resync & Rebuild" action.
- Documentation governance baseline: root `AGENTS.md`, `docs/status.md`, PR template checklist, and CI `docs-guard` enforcement.
- Live deployment log piping for compose/git commands, persisted line-by-line and visible through `/api/v1/apps/:id/logs/stream`.
