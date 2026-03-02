# Status

## Now (Implemented)

- Self-hosted Rust control plane with SQLite state and durable reconciler queue.
- Web dashboard with auth, app import/create, deploy/rollback, domain mapping, logs, and env var management.
- Visual refresh for the dashboard is aligned with the new v0 mock (sidebar-first layout, modern dark styling, and card-based information hierarchy) without changing API-backed workflows.
- Improved scrolling polish now anchors root background/overscroll handling so fast scroll bounce no longer reveals blank top/bottom gaps.
- Dashboard graph widgets now use live telemetry: request traffic series (global/per-app) from Caddy access logs and host resource series from agent heartbeat snapshots.
- Dashboard stat cards now load real values (applications, healthy managed services, domains, and 30d uptime proxy) from backend metrics instead of hardcoded placeholders.
- Access-log ingestion now runs behind `RUSTPLOY_CADDY_ACCESS_LOG_ENABLED`, performs blocking file reads off the async runtime, and caches host lookups to reduce steady DB pressure.
- Agent resource sample timestamps are now clamped to server receive time (bounded future skew) before persistence.
- Added a stronger operations panel with a collapsible "Recent Deployments" card plus a dedicated "Selected Deployment" summary card for condensed status/metadata.
- Dedicated Logs Explorer page (`/logs`) now supports deployment queries and filtering across app/status/source/time window, plus text filtering within loaded deployment logs.
- Logs Explorer query flow now tolerates per-app fetch failures (partial results mode), ignores stale out-of-order query responses, and uses DOM-safe rendering for deployment summaries/results.
- Dashboard domains and environment variable lists now render via text-safe DOM node construction to prevent stored XSS from user-provided values.
- Dashboard app selection reset now closes stale live log `EventSource` streams so old app log polling cannot continue in the background.
- Hardened two-column dashboard layout now uses constrained grid sizing to prevent left/right overlap.
- App rows now wrap long app IDs to avoid overlap with action buttons in narrow cards.
- App list rows now use explicit text/actions column layout to keep metadata and buttons separated under narrow widths.
- Terminal UI (`rustploy-tui`) for SSH-first operations.
- GitHub repository mapping + webhook-triggered deployments with signature verification.
- Compose-first runtime deploys:
  - real clone/build/up/health checks
  - streamed compose/git command output into deployment logs (live in dashboard/TUI via SSE streaming)
  - app runtime routing via Caddy
  - per-app environment variable injection at deploy time
  - manual `force_rebuild` deploy option (no-cache rebuild)
- Domain/TLS management with Caddy (`managed` and `custom` modes).
- API token auth with scoped permissions (`read`, `deploy`, `admin`).
- Public API contract and OpenAPI definition.

## Next (Near-term)

- Improve deployment UX:
  - explicit sync status and last fetched commit in UI
  - clearer rollout timeline and error grouping
- Add richer env var workflow:
  - secret masking in UI list
  - import/export and bulk edit
- Expand operational runbooks with troubleshooting matrices per failure class.
- Tighten CI policy checks for docs and API drift.

## Later (Full Picture)

- Team access model (`maintainer`, `viewer`) beyond single-owner baseline.
- Native non-compose build/deploy pipeline parity with compose workflows.
- More integrations (Git providers, registry workflows, webhooks/events).
- Stronger observability (traces, dashboards, retention policies).

## Known Gaps

- Browser trust warnings for local HTTPS are expected without trusted local certificates.
- Non-compose deploy path is currently lighter-weight than compose runtime path.
- API docs and examples are improving; keep `openapi.yaml` as canonical contract.

## Last Verified

- Date: 2026-03-02
- Commit base: `935ac11`
- Note: dashboard telemetry now includes a new `/api/v1/dashboard/metrics` endpoint plus access-log and heartbeat ingestion so request/resource widgets render live global/per-app data in the main dashboard shell.
