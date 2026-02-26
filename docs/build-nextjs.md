# Next.js Build Strategy (v0.1)

Rustploy should support importing a simple Next.js repository without requiring a user-authored Dockerfile.

## Goals

- Zero-config import for common Next.js apps.
- Automatic package manager detection.
- Consistent, reproducible container image output.

## Detection

A repository is treated as Next.js when:

- `package.json` includes `next` dependency, and
- scripts include `build` and `start`.

Package manager detection order:

1. `pnpm-lock.yaml` -> use `pnpm`
2. `yarn.lock` -> use `yarn`
3. `package-lock.json` -> use `npm`
4. fallback -> `npm`

## Build flow

1. Clone repository at selected commit.
2. Install dependencies with detected package manager.
3. Run `build` script.
4. Build runtime image using standard Rustploy template.
5. Start app with detected package manager `start` script.

## Runtime expectations

- App binds to `PORT` env var.
- Health check defaults to `/` and is user-editable.
- `next.config.js` with `output: "standalone"` is recommended.

## Overrides

- If a `Dockerfile` exists and user selects custom mode, use Dockerfile flow.
- If detection fails, require explicit runtime config in import wizard.
- Repository `rustploy.yaml` values override inferred defaults.
