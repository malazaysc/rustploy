use std::{env, time::SystemTime};

use anyhow::{Context, Result};
use shared::{AgentHeartbeat, AgentRegisterRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};
use uuid::Uuid;

const AGENT_TOKEN_HEADER: &str = "x-rustploy-agent-token";

#[derive(Debug, Clone)]
struct AgentConfig {
    server_addr: String,
    agent_id: Uuid,
    agent_version: String,
    interval_seconds: u64,
    oneshot: bool,
    agent_token: Option<String>,
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

async fn send_heartbeat(config: &AgentConfig) -> Result<()> {
    let heartbeat = AgentHeartbeat {
        agent_id: config.agent_id,
        timestamp_unix_ms: unix_ms_now(),
        agent_version: config.agent_version.clone(),
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
        len = body.as_bytes().len()
    );

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
