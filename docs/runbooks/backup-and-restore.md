# Runbook: Backup and Restore

## Goal

Protect and recover Rustploy control plane and TLS state.

## What to back up

- SQLite database file.
- Caddy certificate/state directory.
- Runtime configuration and secrets files.

## Backup procedure

1. Create consistent SQLite snapshot (or stop server briefly).
2. Archive DB + Caddy data + config files.
3. Encrypt backup archive.
4. Store off-host.
5. Verify backup integrity checksums.

## Restore procedure

1. Provision clean host with matching Rustploy version.
2. Restore DB and Caddy data to expected paths.
3. Restore configuration and secrets.
4. Start services.
5. Validate apps, domains, and deployment history.

## Frequency recommendation

- Daily incremental backups.
- Weekly full backup.
- Quarterly restore drill.
