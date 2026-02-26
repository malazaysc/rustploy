# Roadmap

## Milestone 0: Foundation

- Repo skeleton, docs, CI, and development conventions.
- Core service and agent crates compile and run locally.

## Milestone 1: Single-node deploy core

- App management, deployment lifecycle, health checks, rollback.
- Durable queue and reconciler.

## Milestone 2: GitHub and TLS

- GitHub App connection and webhook deploy triggers.
- Next.js zero-config import and build support.
- Manifest-driven import overrides and managed dependencies.
- Domain mapping and automatic cert management.

## Milestone 3: UX and operations

- Dashboard for deploy status/logs.
- Terminal UI for SSH-first operations.
- Packaging and upgrade flow for self-host users.

## Milestone 4: Integrations and API

- Stable `/api/v1` for automation and external integrations.
- Token lifecycle management and scope enforcement.
- Published OpenAPI docs and usage examples.
