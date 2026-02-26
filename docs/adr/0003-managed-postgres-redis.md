# ADR 0003: Managed Postgres and Redis Dependencies

- Status: Accepted
- Date: 2026-02-26

## Context

Many imported apps require Postgres or Redis. Requiring users to orchestrate these services manually undermines the PaaS goal.

## Decision

Provide managed Postgres and Redis as first-class dependency resources.

- Provision as internal service containers.
- Add persistent storage by default.
- Inject generated URLs via environment variables.

## Consequences

Positive:

- Simpler app onboarding.
- Standardized runtime defaults and health gates.

Tradeoffs:

- More operational responsibilities in the platform.
- Version upgrades and backup strategy must be explicit.

## Follow-up

- Add dependency lifecycle APIs.
- Document backup/restore and upgrade policies.
