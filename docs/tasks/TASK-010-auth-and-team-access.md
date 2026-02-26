# TASK-010: Auth and team access

- Status: Done
- Priority: P2
- Estimate: 5-7 days
- Owner: Unassigned

## Goal

Add authentication baseline and prepare for team collaboration.

## Scope

- Local auth for self-host admin.
- Session management and password reset basics.
- Role model groundwork (`owner`, future `maintainer/viewer`).

## Acceptance criteria

- Admin login required for dashboard/API access.
- Sessions expire and can be revoked.
- Authorization checks exist for app operations.

## Completion notes

- Local admin auth/login, session cookies, and password reset endpoints are implemented.
- Sessions include TTL and explicit revoke/logout support.
- Role-based session scope (`owner`) and token scope checks gate app operations.

## Dependencies

- TASK-002
- TASK-007
