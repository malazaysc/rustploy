# TASK-012: Release engineering

- Status: Done
- Priority: P1
- Estimate: 3-4 days
- Owner: Unassigned

## Goal

Establish repeatable open-source release process.

## Scope

- Versioning and changelog policy.
- Signed release artifacts.
- SBOM generation and publishing.

## Acceptance criteria

- Tagged release pipeline produces installable artifacts.
- Release notes are generated and published.
- Provenance and SBOM assets are attached.

## Completion notes

- Added tag-driven GitHub Actions release workflow.
- Workflow builds server/agent/rustploy-tui binaries and publishes checksums.
- SBOM and provenance attestation artifacts are generated and attached to releases.

## Dependencies

- TASK-001
- TASK-009
- TASK-011
