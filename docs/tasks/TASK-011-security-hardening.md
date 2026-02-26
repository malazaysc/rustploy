# TASK-011: Security hardening

- Status: Done
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

## Completion notes

- Internal error logging now redacts token/session/reset secrets.
- API and webhook endpoints enforce in-memory per-minute rate limits.
- CI includes `cargo audit` and Trivy scans that fail on HIGH/CRITICAL findings.

## Dependencies

- TASK-004
- TASK-008
- TASK-010
