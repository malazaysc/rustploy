# Terminal UI (TUI)

Rustploy should provide a first-party terminal interface for operators working directly on VPS hosts.

## Goals

- Full operational access from terminal-only environments.
- Fast navigation and low resource usage.
- Parity for core workflows with the web dashboard.

## Recommended stack

- `ratatui` for terminal rendering.
- `crossterm` for input and terminal control.
- `tokio` async runtime for API calls and log streaming.

## Core screens (v0.1)

- Apps list with health and last deploy status.
- App detail with environments, domains, and deployment history.
- Live logs viewer via SSE/WebSocket bridge.
- Deployment trigger and rollback actions.
- Token status and API connectivity checks.

## Interaction model

- Keyboard-first navigation (`j/k`, arrows, quick actions).
- Command palette for frequent actions.
- Read-only mode when token scope is limited.

## Delivery model

- Distributed as standalone `rustploy-tui` binary.
- Auth via API token or local session handoff.
- Works over SSH with no browser requirement.
