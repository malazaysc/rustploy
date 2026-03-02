# Status

## Now (Implemented)

- Self-hosted Rust control plane with SQLite state and durable reconciler queue.
- Web dashboard with auth, app import/create, deploy/rollback, domain mapping, logs, and env var management.
- Dashboard visual refresh aligned to the new v0 mock (sidebar-first layout, modern dark styling, and card-based information hierarchy) without changing API-backed workflows.
- Dashboard scrolling polish: root background/overscroll handling now prevents blank top/bottom gaps during fast scroll bounce.
- Dashboard graph placeholders now cover traffic and server resource chart areas so all graph panels are represented before live metrics data is wired in.
- Dashboard operations panel now includes a collapsible "Recent Deployments" card and a dedicated "Selected Deployment" summary card with condensed status/metadata.
- Dedicated Logs Explorer page (`/logs`) now supports deployment queries and filtering across app/status/source/time window, plus text filtering within loaded deployment logs.
- Logs Explorer query flow now tolerates per-app fetch failures (partial results mode) and uses DOM-safe rendering for deployment summaries/results.
- Dashboard layout hardening: two-column panels now use constrained grid sizing to prevent left/right overlap.
- Dashboard app rows now wrap long app IDs to avoid overlap with action buttons in narrow cards.
- Dashboard app list rows now use explicit text/actions column layout to keep metadata and buttons separated under narrow widths.
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

- Date: 2026-03-01
- Commit base: `935ac11`
- Note: dashboard now uses a v0-inspired visual shell (sidebar, stats cards, graph placeholders, and updated panel styling), includes overscroll gap fixes for fast scroll behavior, supports collapsible deployment history with selected-deployment summaries, and pairs with a new `/logs` page for query/filter-based log investigation while preserving structured SSE streaming in the main dashboard.
