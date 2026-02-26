# ADR 0002: Next.js Auto-Build First

- Status: Accepted
- Date: 2026-02-26

## Context

Users want easy GitHub import without writing Dockerfiles. v0.1 needs a concrete low-friction developer path.

## Decision

Support zero-config Next.js import/build as the first auto-build profile.

- Detect framework from `package.json`.
- Support package managers `pnpm`, `yarn`, and `npm`.
- Keep Dockerfile mode as explicit override.

## Consequences

Positive:

- Fast onboarding for common web app use cases.
- Reduced user burden during initial adoption.

Tradeoffs:

- Build subsystem complexity increases.
- Framework-specific assumptions need clear docs.

## Follow-up

- Add integration tests for all supported lockfiles.
- Extend build profiles incrementally after v0.1 stability.
