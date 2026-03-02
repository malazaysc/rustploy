use std::{
    collections::HashSet,
    env,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use shared::{AgentHeartbeat, AgentRegisterRequest, AgentResourceSnapshot};
use sysinfo::{CpuExt, DiskExt, NetworkExt, NetworksExt, System, SystemExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};
use uuid::Uuid;

const AGENT_TOKEN_HEADER: &str = "x-rustploy-agent-token";
const TRACEPARENT_HEADER: &str = "traceparent";

#[derive(Debug, Clone)]
struct AgentConfig {
    server_addr: String,
    agent_id: Uuid,
    agent_version: String,
    interval_seconds: u64,
    oneshot: bool,
    agent_token: Option<String>,
}

#[derive(Debug)]
struct ResourceCollector {
    system: System,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = AgentConfig::from_env()?;

    info!(
        server_addr = %config.server_addr,
        agent_id = %config.agent_id,
        interval_seconds = config.interval_seconds,
        oneshot = config.oneshot,
        "rustploy-agent started"
    );

    if let Err(error) = send_registration(&config).await {
        warn!(%error, "agent registration failed");
    }
    let mut resource_collector = ResourceCollector::new();
    tokio::time::sleep(System::MINIMUM_CPU_UPDATE_INTERVAL).await;

    if config.oneshot {
        send_heartbeat(&config, Some(resource_collector.collect())).await?;
        return Ok(());
    }

    let heartbeat_interval =
        Duration::from_secs(config.interval_seconds).max(System::MINIMUM_CPU_UPDATE_INTERVAL);
    let mut ticker = tokio::time::interval(heartbeat_interval);

    loop {
        ticker.tick().await;
        if let Err(error) = send_heartbeat(&config, Some(resource_collector.collect())).await {
            warn!(%error, "heartbeat failed");
        }
    }
}

impl AgentConfig {
    fn from_env() -> Result<Self> {
        let server_addr =
            env::var("RUSTPLOY_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

        let agent_id = match env::var("RUSTPLOY_AGENT_ID") {
            Ok(value) => Uuid::parse_str(&value)
                .with_context(|| format!("invalid RUSTPLOY_AGENT_ID: {value}"))?,
            Err(_) => Uuid::new_v4(),
        };

        let interval_seconds = env::var("RUSTPLOY_HEARTBEAT_INTERVAL_SECS")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u64>()
            .context("invalid RUSTPLOY_HEARTBEAT_INTERVAL_SECS")?;

        let oneshot = env::var("RUSTPLOY_AGENT_ONESHOT")
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE"))
            .unwrap_or(false);

        let agent_token = env::var("RUSTPLOY_AGENT_TOKEN").ok();

        Ok(Self {
            server_addr,
            agent_id,
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            interval_seconds,
            oneshot,
            agent_token,
        })
    }
}

async fn send_registration(config: &AgentConfig) -> Result<()> {
    let request = AgentRegisterRequest {
        agent_id: config.agent_id,
        agent_version: config.agent_version.clone(),
    };

    send_json_request(
        config,
        "/api/v1/agents/register",
        &request,
        &[200, 201],
        "registration",
    )
    .await
}

async fn send_heartbeat(
    config: &AgentConfig,
    resource: Option<AgentResourceSnapshot>,
) -> Result<()> {
    let heartbeat = AgentHeartbeat {
        agent_id: config.agent_id,
        timestamp_unix_ms: unix_ms_now(),
        agent_version: config.agent_version.clone(),
        resource,
    };

    send_json_request(
        config,
        "/api/v1/agents/heartbeat",
        &heartbeat,
        &[202],
        "heartbeat",
    )
    .await?;

    info!(agent_id = %config.agent_id, "heartbeat accepted");
    Ok(())
}

impl ResourceCollector {
    fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_cpu();
        system.refresh_memory();
        system.refresh_disks_list();
        system.refresh_disks();
        system.refresh_networks_list();
        system.refresh_networks();
        Self { system }
    }

    fn collect(&mut self) -> AgentResourceSnapshot {
        self.system.refresh_cpu();
        self.system.refresh_memory();
        self.system.refresh_disks();
        self.system.refresh_networks();

        let (disk_total_bytes, disk_available_bytes) = aggregate_disk_capacity(self.system.disks());
        let (network_rx_bytes, network_tx_bytes) =
            self.system
                .networks()
                .iter()
                .fold((0u64, 0u64), |(rx, tx), (_name, data)| {
                    (
                        rx.saturating_add(data.total_received()),
                        tx.saturating_add(data.total_transmitted()),
                    )
                });

        AgentResourceSnapshot {
            cpu_percent: self.system.global_cpu_info().cpu_usage() as f64,
            // sysinfo already reports memory in bytes on our pinned version.
            memory_used_bytes: self.system.used_memory(),
            memory_total_bytes: self.system.total_memory(),
            disk_used_bytes: disk_total_bytes.saturating_sub(disk_available_bytes),
            disk_total_bytes,
            network_rx_bytes,
            network_tx_bytes,
        }
    }
}

fn aggregate_disk_capacity(disks: &[sysinfo::Disk]) -> (u64, u64) {
    if let Some(root_disk) = disks
        .iter()
        .find(|disk| disk.mount_point() == std::path::Path::new("/") && disk.total_space() > 0)
    {
        return (root_disk.total_space(), root_disk.available_space());
    }

    let mut seen_devices = HashSet::new();
    let mut total = 0u64;
    let mut available = 0u64;

    for disk in disks {
        let fs_name = std::str::from_utf8(disk.file_system())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if should_skip_filesystem(&fs_name) {
            continue;
        }

        let Some(key) = disk_dedup_key(disk, &fs_name) else {
            continue;
        };
        if !seen_devices.insert(key) {
            continue;
        }

        total = total.saturating_add(disk.total_space());
        available = available.saturating_add(disk.available_space());
    }

    if total == 0 {
        for disk in disks {
            total = total.saturating_add(disk.total_space());
            available = available.saturating_add(disk.available_space());
        }
    }

    (total, available)
}

fn disk_dedup_key(disk: &sysinfo::Disk, fs_name: &str) -> Option<String> {
    let name = disk.name().to_string_lossy().trim().to_ascii_lowercase();
    if fs_name == "btrfs" {
        if !name.is_empty() {
            return Some(format!("btrfs:device:{name}"));
        }
        return Some(format!(
            "btrfs:mount:{}",
            btrfs_mount_group(disk.mount_point())
        ));
    }
    if !name.is_empty() {
        return Some(name);
    }
    let mount_key = disk
        .mount_point()
        .display()
        .to_string()
        .to_ascii_lowercase();
    if mount_key.is_empty() {
        None
    } else {
        Some(mount_key)
    }
}

fn btrfs_mount_group(mount_point: &std::path::Path) -> String {
    let mut segments = mount_point.components().filter_map(|part| match part {
        std::path::Component::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
        _ => None,
    });
    match (segments.next(), segments.next()) {
        (Some(first), Some(second)) => format!("/{first}/{second}"),
        (Some(first), None) => format!("/{first}"),
        _ => "/".to_string(),
    }
}

fn should_skip_filesystem(fs_name: &str) -> bool {
    matches!(
        fs_name,
        "overlay"
            | "tmpfs"
            | "devtmpfs"
            | "squashfs"
            | "ramfs"
            | "aufs"
            | "proc"
            | "procfs"
            | "sysfs"
            | "cgroup"
            | "cgroup2fs"
            | "nsfs"
            | "autofs"
            | "tracefs"
            | "debugfs"
            | "securityfs"
            | "fusectl"
            | "mqueue"
            | "devpts"
    )
}

async fn send_json_request<T: serde::Serialize>(
    config: &AgentConfig,
    path: &str,
    payload: &T,
    allowed_statuses: &[u16],
    action: &str,
) -> Result<()> {
    let body =
        serde_json::to_string(payload).with_context(|| format!("failed to serialize {action}"))?;
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n",
        host = config.server_addr,
        len = body.len()
    );
    let trace_id = Uuid::new_v4().simple().to_string();
    let span_id = &Uuid::new_v4().simple().to_string()[..16];
    request.push_str(&format!(
        "{TRACEPARENT_HEADER}: 00-{trace_id}{trace_id}-{span_id}-01\r\n"
    ));

    if let Some(token) = &config.agent_token {
        request.push_str(&format!("{AGENT_TOKEN_HEADER}: {token}\r\n"));
    }

    request.push_str("\r\n");
    request.push_str(&body);

    let mut stream = tokio::net::TcpStream::connect(&config.server_addr)
        .await
        .with_context(|| format!("failed to connect to {}", config.server_addr))?;

    stream
        .write_all(request.as_bytes())
        .await
        .with_context(|| format!("failed to write {action} request"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .with_context(|| format!("failed to read {action} response"))?;

    let response = String::from_utf8_lossy(&response);
    let status = parse_status_code(&response).context("invalid HTTP status line")?;

    if !allowed_statuses.contains(&status) {
        anyhow::bail!(
            "server rejected {action} with status {status}: {}",
            first_line(&response)
        );
    }

    Ok(())
}

fn parse_status_code(response: &str) -> Option<u16> {
    let mut parts = response.lines().next()?.split_whitespace();
    let _http_version = parts.next()?;
    parts.next()?.parse::<u16>().ok()
}

fn first_line(response: &str) -> &str {
    response.lines().next().unwrap_or("invalid-response")
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{btrfs_mount_group, should_skip_filesystem};
    use std::path::Path;

    #[test]
    fn skips_virtual_and_overlay_filesystems() {
        assert!(should_skip_filesystem("overlay"));
        assert!(should_skip_filesystem("tmpfs"));
        assert!(should_skip_filesystem("proc"));
        assert!(!should_skip_filesystem("ext4"));
        assert!(!should_skip_filesystem("xfs"));
    }

    #[test]
    fn groups_btrfs_subvolume_mounts() {
        assert_eq!(btrfs_mount_group(Path::new("/")), "/");
        assert_eq!(btrfs_mount_group(Path::new("/home")), "/home");
        assert_eq!(
            btrfs_mount_group(Path::new("/mnt/data/subvolumes/app-a")),
            "/mnt/data"
        );
    }
}
