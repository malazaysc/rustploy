# Security

## Baseline controls

- TLS everywhere for user-facing endpoints.
- Encrypted secrets at rest where feasible.
- Signed webhook verification.
- Least-privilege GitHub App permissions.

## Hardening goals

- Strict container resource limits by default.
- Secret redaction in logs.
- Role-based access in roadmap.
- Vulnerability scanning and SBOM in CI.

## Responsible disclosure

- Security reports should be handled privately first.
- Add `SECURITY.md` at repository root before public launch.
