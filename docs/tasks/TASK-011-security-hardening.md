# TASK-011: Security hardening

- Status: Todo
- Priority: P1
- Estimate: 4-6 days
- Owner: Unassigned

## Goal

Raise baseline security posture before broader release.

## Scope

- Secret handling and redaction.
- Webhook and API rate limiting.
- Dependency and container image scanning in CI.

## Acceptance criteria

- Secrets never appear in normal logs.
- Abuse protections enabled for webhook endpoints.
- CI fails on high-severity known vulnerabilities.

## Dependencies

- TASK-004
- TASK-008
- TASK-010
