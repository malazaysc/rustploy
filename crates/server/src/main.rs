use std::net::SocketAddr;

use anyhow::{Context, Result};
use server::{create_router, run_reconciler_loop, AppState};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let state = AppState::from_env().context("failed to initialize server state")?;
    if state.reconciler_enabled() {
        tokio::spawn(run_reconciler_loop(state.clone()));
    }

    let addr = read_bind_addr().context("failed to parse bind address")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind server to {addr}"))?;

    info!(%addr, "rustploy-server listening");

    axum::serve(listener, create_router(state))
        .await
        .context("server exited with error")
}

fn read_bind_addr() -> Result<SocketAddr> {
    std::env::var("RUSTPLOY_SERVER_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse::<SocketAddr>()
        .context("invalid RUSTPLOY_SERVER_ADDR")
}
