# Rustploy

Rustploy is a self-hosted, fully open source deployment platform inspired by Dokploy, built in Rust with a strong focus on low resource consumption.

## Current status

Documentation plus a working control-plane MVP are in place.

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

3. Bootstrap an admin API token (first token can be created without auth):

```bash
curl -sS -X POST http://localhost:8080/api/v1/tokens \
  -H 'content-type: application/json' \
  -d '{"name":"admin","scopes":["admin"]}'
```

4. In another terminal, run one heartbeat from the agent:

```bash
RUSTPLOY_AGENT_ONESHOT=true cargo run -p agent
```

5. Run terminal UI:

```bash
RUSTPLOY_API_TOKEN=<token-from-step-3> cargo run -p tui
```

## Run without local Rust

Use Docker Compose to build and run Rustploy in containers:

```bash
docker compose up --build
```

This starts:

- `server` on `http://localhost:8080`
- `agent` sending periodic heartbeats to `server`

Useful checks:

```bash
curl http://localhost:8080/api/v1/health
curl http://localhost:8080/api/v1/agents
curl -X POST http://localhost:8080/api/v1/apps -H 'content-type: application/json' -d '{"name":"demo"}'
```

To enforce shared-token auth between agent and server, set the same `RUSTPLOY_AGENT_TOKEN` value in both services inside `docker-compose.yml`.

## API highlights

- Repo import and detection:

```bash
curl -X POST http://localhost:8080/api/v1/apps/import \
  -H "Authorization: Bearer <admin-token>" \
  -H 'content-type: application/json' \
  -d '{
    "repository": {
      "provider": "github",
      "owner": "acme",
      "name": "next-app",
      "default_branch": "main"
    },
    "source": { "branch": "main" },
    "build_mode": "auto"
  }'
```

- Rollback last healthy source:

```bash
curl -X POST http://localhost:8080/api/v1/apps/<app-id>/rollback \
  -H "Authorization: Bearer <deploy-or-admin-token>"
```

- GitHub webhook signature verification:

Set `RUSTPLOY_GITHUB_WEBHOOK_SECRET` on the server and point your webhook to:

`POST /api/v1/integrations/github/webhook`

## TUI commands

`rustploy-tui` supports:

- `apps` / `refresh`
- `deployments <app_index>`
- `deploy <app_index> [source_ref]`
- `rollback <app_index>`
- `help`
- `quit`

## Documentation index

See [`docs/README.md`](./docs/README.md).

## License

Apache License 2.0. See [`LICENSE`](./LICENSE).
