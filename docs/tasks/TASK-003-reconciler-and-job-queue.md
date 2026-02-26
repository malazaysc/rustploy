# TASK-003: Reconciler and durable job queue

- Status: Todo
- Priority: P0
- Estimate: 5-7 days
- Owner: Unassigned

## Goal

Build desired/actual state reconciliation with a durable SQLite-backed job queue.

## Scope

- Schema for jobs and deployment state.
- Reconciler loop and retry/backoff logic.
- Idempotent job execution contract.

## Acceptance criteria

- Reconciler creates jobs from desired state changes.
- Failed jobs retry with bounded backoff.
- Restarting server does not lose queued jobs.

## Dependencies

- TASK-001
- TASK-002
