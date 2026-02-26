# TASK-013: Public API and token access

- Status: Todo
- Priority: P0
- Estimate: 5-7 days
- Owner: Unassigned

## Goal

Expose Rustploy functionality via stable API endpoints for automation and integrations.

## Scope

- Implement `/api/v1` app and deployment endpoints.
- Add PAT and service token issuance with scope model.
- Add token revoke, expiration, and audit trail.
- Generate and publish OpenAPI spec.

## Acceptance criteria

- API clients can manage apps and trigger deployments with token auth.
- Token scope enforcement is validated in tests.
- Revoked tokens fail immediately.
- OpenAPI spec is generated from code and published.

## Dependencies

- TASK-002
- TASK-005
- TASK-010
- TASK-011
