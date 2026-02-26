# Rustploy

Rustploy is a self-hosted, fully open source deployment platform inspired by Dokploy, built in Rust with a strong focus on low resource consumption.

## Current status

Documentation plus initial Rust workspace bootstrap are in place.

- Product and architecture docs: [`docs/`](./docs)
- Ticket backlog: [`docs/tasks/`](./docs/tasks)
- API planning: [`docs/api.md`](./docs/api.md)
- OpenAPI source: [`openapi.yaml`](./openapi.yaml)
- Runbooks: [`docs/runbooks/`](./docs/runbooks)

## Goals

- Single-node deployment platform for small VPS instances.
- GitHub-connected delivery flow.
- Managed HTTPS and domains.
- Durable deployments with rollback.
- First-class web dashboard and terminal UI (TUI).
- Low memory and CPU overhead.

## Local development quickstart

1. Build and test everything:

```bash
cargo test --workspace
```

2. Run server (defaults to `0.0.0.0:8080`):

```bash
cargo run -p server
```

3. In another terminal, run one heartbeat from the agent:

```bash
RUSTPLOY_AGENT_ONESHOT=true cargo run -p agent
```

4. Check server health from the TUI bootstrap:

```bash
cargo run -p tui
```

## Documentation index

See [`docs/README.md`](./docs/README.md).

## License

Apache License 2.0. See [`LICENSE`](./LICENSE).
