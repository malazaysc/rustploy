# TASK-016: rustploy.yaml manifest support

- Status: In Progress
- Priority: P0
- Estimate: 4-6 days
- Owner: Unassigned

## Goal

Support repository-level `rustploy.yaml` to control build/runtime/dependency intent.

## Scope

- Implement manifest parser and validation for schema v1.
- Merge manifest values with app settings using precedence rules.
- Surface manifest warnings in import and deployment logs.

## Acceptance criteria

- Manifest values are applied on import and redeploy.
- Invalid manifest returns structured validation errors.
- UI/TUI displays effective config after merge.

## Dependencies

- TASK-004
- TASK-013
- TASK-015
