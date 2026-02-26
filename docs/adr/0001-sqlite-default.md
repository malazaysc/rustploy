# ADR 0001: SQLite as Default Database

- Status: Accepted
- Date: 2026-02-26

## Context

Rustploy v0.1 targets low-resource single-node self-hosted deployments. The control plane needs durable state with minimal operational overhead.

## Decision

Use SQLite as the default and only required database for v0.1.

- Enable WAL mode.
- Use one persistent DB file on disk.
- Keep schema compatible with future Postgres migration.

## Consequences

Positive:

- Simple installation with no external DB service.
- Lower memory and CPU footprint.
- Fewer moving parts for self-hosted users.

Tradeoffs:

- Limited write concurrency compared to Postgres.
- Future multi-node control plane will require a different primary store.

## Follow-up

- Define backup and restore runbook.
- Keep DB abstraction boundaries explicit.
