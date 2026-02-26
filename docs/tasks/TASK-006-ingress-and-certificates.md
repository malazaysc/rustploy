# TASK-006: Ingress and certificates

- Status: Done
- Priority: P0
- Estimate: 4-6 days
- Owner: Unassigned

## Goal

Provide domain routing with managed TLS certificates.

## Scope

- Integrate Caddy as default ingress for v0.1.
- HTTP-01 certificate automation.
- Support BYO certificate configuration.

## Acceptance criteria

- App can be mapped to custom domain.
- Valid certificate is provisioned and renewed automatically.
- Certificate state survives restart via persistent volume.

## Completion notes

- Caddyfile is generated from domain mappings and updated on startup/domain changes.
- Domain API supports managed TLS and BYO cert/key paths.
- Compose includes persistent Caddy state volumes for cert/config continuity.

## Dependencies

- TASK-002
- TASK-003
