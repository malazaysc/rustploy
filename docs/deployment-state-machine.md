# Deployment State Machine (v0.1)

This defines the currently implemented deployment lifecycle states and transitions.

## States

- `queued`: deployment accepted and waiting for worker.
- `deploying`: deployment job running (clone/build/run/health checks).
- `retrying`: previous deploy attempt failed but job will retry.
- `healthy`: health check succeeded and deployment is active.
- `failed`: deployment failed and did not become healthy.

## Allowed transitions

- `queued -> deploying`
- `deploying -> healthy | retrying | failed`
- `retrying -> deploying | failed`

## Retry policy

- Failed deploy attempts transition to `retrying` while attempts remain.
- Retry delay uses bounded exponential backoff.
- Each attempt appends deployment logs and preserves prior failure context.

## Terminal states

- `healthy`
- `failed`
