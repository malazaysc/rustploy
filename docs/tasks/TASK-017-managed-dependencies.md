# TASK-017: Managed Postgres and Redis dependencies

- Status: Done
- Priority: P0
- Estimate: 7-10 days
- Owner: Unassigned

## Goal

Provide managed dependency services for apps that require Postgres and/or Redis.

## Scope

- Provision Postgres/Redis service containers with persistent volumes.
- Generate and rotate credentials with secret storage.
- Inject `DATABASE_URL` and `REDIS_URL` into app runtime env.
- Enforce startup ordering and health checks.

## Acceptance criteria

- App with enabled Postgres/Redis dependencies deploys successfully.
- Dependency health gates app start.
- Dependency services are internal-only by default.
- Credentials never appear in normal logs.

## Completion notes

- Dependency profile (`postgres`/`redis`) is merged from import settings + manifest.
- Managed dependency records are provisioned with generated credentials and internal hostnames.
- Deploy execution enforces dependency readiness before marking deployment healthy.
- Logs report dependency readiness without printing connection credentials.

## Dependencies

- TASK-003
- TASK-006
- TASK-011
- TASK-016
