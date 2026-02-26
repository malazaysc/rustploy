use std::{env, time::SystemTime};

use anyhow::{Context, Result};
use shared::AgentHeartbeat;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Clone)]
struct AgentConfig {
    server_addr: String,
    agent_id: Uuid,
    interval_seconds: u64,
    oneshot: bool,
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

    if config.oneshot {
        send_heartbeat(&config).await?;
        return Ok(());
    }

    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(config.interval_seconds));

    loop {
        ticker.tick().await;
        if let Err(error) = send_heartbeat(&config).await {
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

        Ok(Self {
            server_addr,
            agent_id,
            interval_seconds,
            oneshot,
        })
    }
}

async fn send_heartbeat(config: &AgentConfig) -> Result<()> {
    let heartbeat = AgentHeartbeat {
        agent_id: config.agent_id,
        timestamp_unix_ms: unix_ms_now(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let body = serde_json::to_string(&heartbeat).context("failed to serialize heartbeat")?;
    let request = format!(
        "POST /api/v1/agents/heartbeat HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        host = config.server_addr,
        len = body.as_bytes().len()
    );

    let mut stream = tokio::net::TcpStream::connect(&config.server_addr)
        .await
        .with_context(|| format!("failed to connect to {}", config.server_addr))?;

    stream
        .write_all(request.as_bytes())
        .await
        .context("failed to write heartbeat request")?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .context("failed to read heartbeat response")?;

    let response = String::from_utf8_lossy(&response);

    if !response.starts_with("HTTP/1.1 202") && !response.starts_with("HTTP/1.0 202") {
        anyhow::bail!("server rejected heartbeat: {}", first_line(&response));
    }

    info!(agent_id = %config.agent_id, "heartbeat accepted");
    Ok(())
}

fn first_line(response: &str) -> &str {
    response.lines().next().unwrap_or("invalid-response")
}

fn unix_ms_now() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis()
}
