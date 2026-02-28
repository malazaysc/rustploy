# rustploy.yaml Manifest (v1)

`rustploy.yaml` is a repository-level manifest used during import to influence app naming, build mode, and managed dependency hints.

## Example

```yaml
version: 1
app:
  name: covachapp
build:
  mode: compose
dependencies:
  postgres:
    enabled: true
  redis:
    enabled: false
```

## Supported keys (v0.1)

- `version`: required, must be `1`.
- `app.name`: optional app name override.
- `build.mode`: optional, one of `auto`, `dockerfile`, `compose`.
- `dependencies.postgres.enabled`: optional boolean.
- `dependencies.redis.enabled`: optional boolean.

Unknown keys are currently ignored.

## Precedence

1. Import/UI-supplied values.
2. `rustploy.yaml` in repository.
3. Auto-detected defaults.
