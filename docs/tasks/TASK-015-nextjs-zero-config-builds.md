# TASK-015: Next.js zero-config builds and package manager support

- Status: Todo
- Priority: P0
- Estimate: 6-9 days
- Owner: Unassigned

## Goal

Allow users to import and deploy common Next.js repositories from GitHub without writing a Dockerfile.

## Scope

- Detect Next.js projects from `package.json`.
- Detect and support `pnpm`, `yarn`, and `npm` lockfiles.
- Build and package app into deployable runtime image.
- Expose import wizard options for auto mode and Dockerfile override.

## Acceptance criteria

- Imported Next.js repo deploys successfully in auto mode.
- `pnpm`, `yarn`, and `npm` projects all build in integration tests.
- Build logs clearly show detected package manager and steps.
- Users can switch to Dockerfile mode when needed.

## Dependencies

- TASK-004
- TASK-005
- TASK-007
