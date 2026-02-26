# TASK-007: Web dashboard UI

- Status: Done
- Priority: P1
- Estimate: 7-10 days
- Owner: Unassigned

## Goal

Deliver a polished and efficient web UI for app/deploy operations.

## Scope

- Rust server-rendered UI (Leptos + Axum or Dioxus fullstack).
- Views for apps, deployments, logs, and domains.
- Real-time status updates using SSE or WebSocket.
- Build against the same `/api/v1` contract used by TUI.

## Acceptance criteria

- User can create app and trigger deployment from UI.
- Deployment status updates in real time.
- UI remains usable on mobile and desktop.

## Completion notes

- Rust server now serves login and dashboard pages at `/`.
- Dashboard supports app create/import, deploy/rollback, domain management, and log viewing.
- Real-time log/status updates are delivered through SSE log stream plus periodic refresh.

## Dependencies

- TASK-002
- TASK-004
- TASK-005
- TASK-006
- TASK-013
