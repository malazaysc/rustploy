# TASK-014: Terminal UI (TUI)

- Status: Todo
- Priority: P1
- Estimate: 6-8 days
- Owner: Unassigned

## Goal

Provide a first-party terminal UI for operating Rustploy in SSH and low-bandwidth environments.

## Scope

- Create `rustploy-tui` binary with keyboard-first navigation.
- Implement views for apps, deployments, domains, and live logs.
- Support deploy and rollback actions through `/api/v1`.
- Handle token-based authentication with scoped permissions.

## Acceptance criteria

- Operator can list apps and inspect deploy status from TUI.
- Operator can trigger deploy and rollback actions from TUI.
- Live logs stream is visible in terminal session.
- TUI runs reliably in common SSH terminal sizes.

## Dependencies

- TASK-007
- TASK-013
