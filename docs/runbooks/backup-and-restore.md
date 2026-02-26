# Runbook: Backup and Restore

## Goal

Protect and recover Rustploy control plane and TLS state.

## What to back up

- SQLite database file.
- Caddy certificate/state directory.
- Runtime configuration and secrets files.

## Backup procedure

1. Stop stack for a consistent snapshot:
   - `docker compose stop server agent caddy`
2. Archive state:
   - `tar -czf rustploy-backup.tgz data/`
   - Include compose volumes for Caddy (`rustploy-caddy`, `rustploy-caddy-config`) if external volume paths are used.
3. Restart services:
   - `docker compose start server agent caddy`
4. Encrypt/archive off-host according to your policy.
5. Record checksum:
   - `sha256sum rustploy-backup.tgz > rustploy-backup.tgz.sha256`

## Restore procedure

1. Provision clean host with matching Rustploy version.
2. Stop stack on target host if already running.
3. Restore backup archive contents to expected paths:
   - `tar -xzf rustploy-backup.tgz`
4. Start services:
   - `docker compose up -d`
5. Validate restore:
   - Login succeeds with admin account.
   - Imported apps and deployment history are present.
   - Existing domain mappings are present and Caddy serves certificates.

## Frequency recommendation

- Daily incremental backups.
- Weekly full backup.
- Quarterly restore drill.
