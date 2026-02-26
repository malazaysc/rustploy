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

- Command-driven app list and deployment history views.
- Repository import command for GitHub-based app onboarding.
- Effective config view (manifest + detected values).
- Deployment trigger and rollback actions.
- API connectivity and auth checks through token-backed requests.

## Interaction model

- Keyboard-only command loop for SSH sessions.
- Quick commands for `apps`, `import`, `config`, `deployments`, `domains`, `deploy`, and `rollback`.
- Deployment log access with `logs <app_index> [deployment_index]` and `watch-logs <app_index> [deployment_index]`.
- Scope-aware behavior (`read` token cannot deploy).

## Delivery model

- Distributed as standalone `rustploy-tui` binary.
- Auth via API token or local session handoff.
- Works over SSH with no browser requirement.
