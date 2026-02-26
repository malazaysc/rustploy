# TASK-002: Control plane and agent skeleton

- Status: Done
- Priority: P0
- Estimate: 4-5 days
- Owner: Unassigned

## Goal

Implement API skeleton and agent heartbeat with secure server-agent communication.

## Scope

- API routes for apps/deployments (stubbed behavior).
- Agent registration + heartbeat endpoint.
- Shared protocol types crate.

## Acceptance criteria

- Agent registers and sends heartbeat every interval.
- Server tracks agent status.
- Integration test covers heartbeat lifecycle.

## Dependencies

- TASK-001
