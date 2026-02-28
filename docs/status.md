# Status

## Now (Implemented)

- Self-hosted Rust control plane with SQLite state and durable reconciler queue.
- Web dashboard with auth, app import/create, deploy/rollback, domain mapping, logs, and env var management.
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

- Date: 2026-02-28
- Commit base: `d326974`
- Note: dashboard reflects `queued/building` deployment state immediately; SSE log stream emits structured JSON payloads, escapes carriage returns, and clears stale output on deployment switches.
