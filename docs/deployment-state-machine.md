# Deployment State Machine (v0.1)

This defines deployment lifecycle states and allowed transitions.

## States

- `queued`: deployment accepted and waiting for worker.
- `preparing`: repository checkout and build context setup.
- `building`: container image build in progress.
- `pushing`: image push to registry (if required).
- `pulling`: runtime host pulling target image.
- `starting`: container start/restart in progress.
- `healthy`: health check succeeded and deployment is active.
- `failed`: deployment failed and did not become healthy.
- `rolling_back`: rollback operation in progress.
- `rolled_back`: previous healthy deployment restored.

## Allowed transitions

- `queued -> preparing`
- `preparing -> building | failed`
- `building -> pushing | pulling | failed`
- `pushing -> pulling | failed`
- `pulling -> starting | failed`
- `starting -> healthy | failed`
- `failed -> rolling_back | terminal`
- `rolling_back -> rolled_back | failed`

## Retry policy

- Transient failures in `preparing/building/pushing/pulling` retry with bounded exponential backoff.
- `starting` retries only for crash-loop threshold window.
- Retries produce audit events and preserve prior logs.

## Terminal states

- `healthy`
- `failed` (when no rollback requested)
- `rolled_back`
