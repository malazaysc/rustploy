# Agent Working Agreement

This file defines mandatory documentation updates for coding agents working in this repository.

## Definition of done

When a change affects behavior, APIs, deployment/runtime, or operations, the same pull request must update documentation.

Required updates:

- Always update:
  - `docs/status.md`
  - `CHANGELOG.md`
- Update API docs when API or wire behavior changes:
  - `openapi.yaml`
  - `docs/api-contract.md`
  - `docs/api.md`

## Change classes that require doc updates

- Any `crates/**` production code change (excluding doc-only edits).
- Any migration change in `crates/server/migrations/**`.
- Any change to runtime packaging or execution:
  - `Dockerfile`
  - `docker-compose.yml`
- Any change to routing, auth, deploy semantics, or manifest parsing.

## PR checklist expectation

Agents must complete the documentation checklist in `.github/PULL_REQUEST_TEMPLATE.md`.

## Override policy

Use `[skip-docs-guard]` in the commit message only for pure refactors/tests with zero user-visible behavior change.
If used, the PR description must clearly explain why docs were not updated.
