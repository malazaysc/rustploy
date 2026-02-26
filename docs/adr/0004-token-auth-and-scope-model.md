# ADR 0004: Token Authentication and Scope Model

- Status: Accepted
- Date: 2026-02-26

## Context

Rustploy must support Web UI, TUI, and external automation. A consistent auth model is required for API safety and usability.

## Decision

Use token-based API authentication with scoped permissions.

- Token types: personal access token and service token.
- Store hashed tokens only.
- Support expiration and explicit revocation.
- Enforce scopes (`read`, `deploy`, `admin`).

## Consequences

Positive:

- Works well for CI/CD and terminal automation.
- Principle of least privilege is enforceable.

Tradeoffs:

- Token lifecycle and UX require careful design.
- Scope checks must be comprehensive and test-covered.

## Follow-up

- Publish scope matrix by endpoint in API docs.
- Add audit events for token creation and usage.
