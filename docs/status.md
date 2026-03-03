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
- Caddy host parsing for telemetry now handles IPv6 literals and drops empty host values to avoid malformed bucket attribution.
- Dashboard metrics refresh now drops stale async responses when selected app context changes, preventing out-of-order widget renders.
- Request Traffic empty-state messaging now uses a fixed-position HTML overlay in the chart shell, preventing stretched/oversized text artifacts from SVG scaling.
- Agent memory telemetry collection now records sysinfo byte values directly (no extra scaling), so Server Resources memory readings align with actual host totals/usages.
- Agent disk telemetry now de-duplicates repeated filesystem/device entries and skips virtual filesystems, reducing inflated disk-capacity readings in containerized environments.
- Agent heartbeat acceptance now treats optional resource-snapshot persistence as best-effort (logs on failure without rejecting the heartbeat), and caddy log polling now uses capped per-read bytes to avoid ingestion memory spikes.
- Dashboard telemetry error rendering now explicitly clears stale stat/chart/gauge values, and managed-service TCP reachability probes run with bounded concurrency for steadier metrics latency.
- Dashboard summary now reports managed-service reachability (TCP probe) instead of raw DB health flags, reducing false-positive "healthy" database counts.
- Dashboard stat-card and routing-copy labels now distinguish deployment history from current runtime liveness to avoid "healthy but offline" confusion.
- Added `GET /api/v1/apps/{app_id}/runtime` runtime probe and wired dashboard routing panel to display live runtime online/offline state separately from deployment history.
- Added project-scoped container inventory APIs (`GET /api/v1/apps/{app_id}/containers` and `GET /api/v1/apps/{app_id}/containers/{container_id}`) to expose runtime container status, port mappings, and detailed metadata with secret env-value masking.
- Container inventory internals now tolerate explicit `null` collection fields from `docker inspect`, execute Docker CLI queries on blocking workers with request timeouts, and return `409` for ambiguous container-id prefix lookups.
- Added per-container logs APIs for compose runtime containers:
  - `GET /api/v1/apps/{app_id}/containers/{container_id}/logs`
  - `GET /api/v1/apps/{app_id}/containers/{container_id}/logs/stream`
  - `GET /api/v1/apps/{app_id}/containers/{container_id}/logs/download`
- Container logs now support reconnect-safe `since` cursors, optional `until` windows, case-insensitive text filtering (`contains`), bounded tail defaults/caps, and download-friendly plain-text responses while preserving original line whitespace in API payloads.
- Container logs cursor advancement now remains monotonic and advances from parsed timestamps even when filtered lines are not returned, avoiding repeated scans/replay loops with `contains` filters.
- Live container log streams now emit one reset frame on transient poll failures and suppress repeated failure events until a successful recovery read.
- Dashboard project workspace now exposes a `Containers` tab with:
  - container status table (status, ports, uptime, restart count, image),
  - selected-container metadata (health/networks/mounts/labels/command),
  - logs viewer controls (contains filter, since/until range, live tail reconnect, and download),
  - automatic container refresh every 5 seconds plus manual refresh.
- Containers-tab follow-up hardening now caps live-log `<pre>` growth, adds keyboard row selection (`Enter`/`Space`), and prevents overlapping background refresh calls when the tab is hidden.
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

- Date: 2026-03-03
- Commit base: `93ab1c5`
- Note: dashboard now includes a project `Containers` tab that consumes existing container inventory/detail/log APIs, with live-log reconnect and explicit empty/loading/error states.
