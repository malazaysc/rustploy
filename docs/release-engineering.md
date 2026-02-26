# Release Engineering

## Versioning and changelog

- Semantic versioning (`vMAJOR.MINOR.PATCH`).
- Changelog follows Keep a Changelog in `CHANGELOG.md`.
- Release notes are generated from merged PRs/commits.

## Release pipeline

`/.github/workflows/release.yml` on tag push:

1. Build `rustploy-server`, `rustploy-agent`, and `rustploy-tui`.
2. Generate `SHA256SUMS`.
3. Generate SPDX SBOM (`sbom.spdx.json`).
4. Attest build provenance using GitHub attestation.
5. Publish GitHub release assets.

## Signing and provenance

- Provenance attestation is attached via `actions/attest-build-provenance`.
- Checksums are published for artifact verification.

## Expected artifacts

- `rustploy-server`
- `rustploy-agent`
- `rustploy-tui`
- `SHA256SUMS`
- `sbom.spdx.json`
