use anyhow::{Context, Result};
use shared::HealthResponse;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<()> {
    let server_addr =
        std::env::var("RUSTPLOY_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    let status = fetch_health(&server_addr).await?;

    println!("rustploy-tui bootstrap ready");
    println!("server: {server_addr}");
    println!("health: {}", status.status);

    Ok(())
}

async fn fetch_health(server_addr: &str) -> Result<HealthResponse> {
    let request = format!(
        "GET /api/v1/health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n",
        host = server_addr
    );

    let mut stream = tokio::net::TcpStream::connect(server_addr)
        .await
        .with_context(|| format!("failed to connect to {server_addr}"))?;

    stream
        .write_all(request.as_bytes())
        .await
        .context("failed to write health request")?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .context("failed to read health response")?;

    let response = String::from_utf8_lossy(&response);
    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .context("missing HTTP response body")?;

    serde_json::from_str::<HealthResponse>(body).context("invalid health response payload")
}
