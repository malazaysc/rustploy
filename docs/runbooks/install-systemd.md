# Runbook: Static Binary + systemd Install

## Goal

Install Rustploy binaries directly on a Linux host without Docker.

## Files

- Unit templates: `packaging/systemd/rustploy-server.service`, `packaging/systemd/rustploy-agent.service`
- Binaries: `rustploy-server`, `rustploy-agent`, `rustploy-tui`

## Steps

1. Create service user and data dir:
   - `sudo useradd --system --home /var/lib/rustploy --shell /usr/sbin/nologin rustploy`
   - `sudo mkdir -p /var/lib/rustploy /opt/rustploy`
2. Install binaries into `/usr/local/bin`.
3. Copy unit files into `/etc/systemd/system/` and adjust env vars if needed.
4. Reload and enable:
   - `sudo systemctl daemon-reload`
   - `sudo systemctl enable --now rustploy-server rustploy-agent`
5. Verify:
   - `systemctl status rustploy-server`
   - `curl http://127.0.0.1:8080/api/v1/health`

## Notes

- For HTTPS/domain routing, run Caddy separately and point it to Rustploy.
- Use `rustploy-tui` over SSH for CLI operations.
