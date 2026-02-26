# Product Requirements (v0.1)

## Problem

Teams want Dokploy-like simplicity without heavy infrastructure and with full self-host control.

## Target users

- Indie builders and small teams.
- Agencies managing multiple small apps.
- Operators running small VPS fleets.

## Primary jobs to be done

- Connect a GitHub repo and deploy quickly.
- Import a simple Next.js repository without writing Dockerfile boilerplate.
- Auto-deploy on push for chosen branches.
- Expose apps with HTTPS and stable domains.
- Observe logs and basic health/metrics.
- Roll back safely to previous versions.
- Operate deployments from SSH-only terminal sessions.

## Functional requirements

- App CRUD and environment variable management.
- Deployment from container image (v0.1), Git-triggered image updates.
- Health checks, restart policy, and rollback.
- Domain mapping and managed TLS.
- GitHub App integration with webhook-driven deploy triggers.
- Zero-config Next.js build pipeline with package manager detection.
- Package manager support for `npm`, `pnpm`, and `yarn`.
- `rustploy.yaml` manifest for repository deployment intent.
- Managed dependency services for Postgres and Redis.
- Public API access for app/deploy operations.
- Personal and service tokens with scoped permissions.
- Web dashboard and terminal UI (TUI) for operations.
- Role model: single owner in v0.1, team access in v0.2.

## Non-functional requirements

- Memory footprint suitable for small VPS hosts.
- Durable operations via local persistent state.
- Clear audit trail of deploy actions.
- No mandatory external queue/cache dependencies.
