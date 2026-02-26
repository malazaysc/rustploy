# Operations

## Runtime assumptions

- Single Linux host for v0.1.
- Public ports 80 and 443 for managed TLS.
- Persistent volume for database and TLS state.

## Backups

- Backup SQLite database on schedule.
- Backup Caddy data directory.
- Validate restore quarterly.

## Incident basics

- Keep a deploy/event timeline per app.
- Support manual rollback to last healthy deployment.
- Surface certificate expiration and webhook failures.
