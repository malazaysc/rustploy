# TASK-001: Project bootstrap

- Status: Done
- Priority: P0
- Estimate: 2-3 days
- Owner: Unassigned

## Goal

Initialize workspace crates, local development tooling, and CI pipeline.

## Scope

- Rust workspace with `server`, `agent`, and shared crate.
- Basic API health endpoint.
- Lint/test/format checks in CI.

## Acceptance criteria

- `cargo test` passes in CI.
- `server` and `agent` binaries run locally.
- README has quickstart commands.

## Dependencies

- None
