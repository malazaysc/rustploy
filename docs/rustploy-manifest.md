# rustploy.yaml Manifest (v1)

`rustploy.yaml` is a repository-level manifest that describes deployment intent for Rustploy.

## Example

```yaml
version: 1
app:
  name: web
  port: 3000
  healthcheck_path: /healthz
  start_command: null
build:
  mode: auto
  profile: nextjs-standalone-v1
  dockerfile_path: null
dependencies:
  postgres:
    enabled: true
    version: "16"
    storage_gb: 10
  redis:
    enabled: true
    version: "7"
    storage_gb: 2
migrations:
  enabled: true
  command: pnpm migrate:deploy
deploy:
  auto_deploy_branch: main
  strategy: rolling
```

## Rules

- `version` is required and currently must be `1`.
- `app.port` is required if auto-detection cannot infer it.
- `build.mode` supports `auto` or `dockerfile`.
- If `build.mode=dockerfile`, `dockerfile_path` may be set.
- Dependencies are optional; disabled dependencies are ignored.
- Unknown keys are ignored with warning in v0.1.

## Precedence

1. UI/TUI overrides saved in Rustploy app settings.
2. `rustploy.yaml` in repository.
3. Auto-detected defaults.
