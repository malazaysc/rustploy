# TASK-004: GitHub App integration

- Status: Done
- Priority: P0
- Estimate: 6-8 days
- Owner: Unassigned

## Goal

Support repository connection and webhook-driven deployment using a GitHub App.

## Scope

- GitHub App setup flow docs + env configuration.
- Store installation and repo mapping.
- Verify webhook signatures.
- Trigger deploy jobs on configured branch pushes.

## Acceptance criteria

- User can connect a repo via GitHub App installation.
- Push to configured branch enqueues deployment job.
- Invalid webhook signatures are rejected.

## Dependencies

- TASK-002
- TASK-003
