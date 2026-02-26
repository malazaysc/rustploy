# TASK-005: Build and deploy pipeline

- Status: Todo
- Priority: P1
- Estimate: 4-6 days
- Owner: Unassigned

## Goal

Define and implement image-based deployment flow with rollback support.

## Scope

- v0.1 deploy prebuilt images.
- Add auto-build path for supported app templates (Next.js first).
- Track deployment history by image tag and commit SHA.
- Rollback API and execution flow.

## Acceptance criteria

- Deploying new image updates running app.
- Auto-build flow produces runnable image for supported templates.
- Rollback to previous deployment works.
- Deployment timeline visible via API.

## Dependencies

- TASK-003
- TASK-004
- TASK-015
