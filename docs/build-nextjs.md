# Next.js Import Detection (v0.1)

Rustploy supports zero-config import detection for common Next.js repositories.

## Detection rules

A repository is classified as Next.js when:

- `package.json` includes a `next` dependency.
- scripts include both `build` and `start`.

Package manager detection order:

1. `pnpm-lock.yaml` -> `pnpm`
2. `yarn.lock` -> `yarn`
3. `package-lock.json` -> `npm`
4. fallback -> `npm`

## Current behavior

- Import stores detected framework/package manager/build profile in app config.
- If a compose file is present, Rustploy deploys through Compose runtime.
- If build mode is non-compose, v0.1 deployment currently records build-step logs and marks deployment state; a full native non-compose builder is future work.

## Runtime notes

- Next.js apps should honor `PORT` from environment in container runtime.
- `next.config.js` with `output: "standalone"` is recommended for containerized runs.
