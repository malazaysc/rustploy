# TASK-008: Observability and logs

- Status: Done
- Priority: P1
- Estimate: 4-5 days
- Owner: Unassigned

## Goal

Add baseline metrics, tracing, and centralized app logs.

## Scope

- Prometheus metrics endpoint.
- OpenTelemetry tracing hooks.
- Log stream from agent to server/UI.

## Acceptance criteria

- Key lifecycle metrics exposed.
- Trace context preserved across server/agent actions.
- Users can view recent app logs from dashboard.

## Completion notes

- `GET /metrics` exposes Prometheus-formatted counters/gauges.
- Agent forwards `traceparent`; server logs trace context on register/heartbeat.
- Deployment logs are persisted and exposed via API and SSE stream.

## Dependencies

- TASK-002
- TASK-003
