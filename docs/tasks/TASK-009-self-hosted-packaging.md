# TASK-009: Self-hosted packaging

- Status: Done
- Priority: P0
- Estimate: 4-6 days
- Owner: Unassigned

## Goal

Make installation and upgrades straightforward for self-host users.

## Scope

- Docker Compose bundle for `server`, `agent`, `sqlite`, and `caddy`.
- Optional static binary install + systemd units.
- Distribute `rustploy-tui` binary in release artifacts.
- Upgrade and backup documentation.

## Acceptance criteria

- Fresh install works from a single documented path.
- Upgrade path preserves app and certificate state.
- Restore steps validated in local test.

## Completion notes

- Compose bundle now includes `server`, `agent`, `sqlite`, and `caddy`.
- Added static binary/systemd unit templates and install runbook.
- Backup/restore and upgrade runbooks document state preservation and validation.

## Dependencies

- TASK-001
- TASK-006
