use anyhow::{Context, Result};
use shared::{
    AppListResponse, AppSummary, CreateDeploymentAccepted, CreateDeploymentRequest,
    DeploymentListResponse, DeploymentLogsResponse, DeploymentSummary, DomainListResponse,
    DomainSummary, EffectiveAppConfigResponse, HealthResponse, ImportAppRequest, ImportAppResponse,
    RepositoryRef, SourceRef,
};
use std::io::{self, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

#[derive(Debug, Clone)]
struct TuiConfig {
    server_addr: String,
    api_token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = TuiConfig {
        server_addr: std::env::var("RUSTPLOY_SERVER_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_string()),
        api_token: std::env::var("RUSTPLOY_API_TOKEN").ok(),
    };

    let status = fetch_health(&config).await?;
    println!("rustploy-tui");
    println!("server: {}", config.server_addr);
    println!("health: {}", status.status);
    print_help();

    let mut apps = fetch_apps(&config).await.unwrap_or_default();
    print_apps(&apps);

    loop {
        print!("rustploy> ");
        io::stdout().flush().context("failed to flush prompt")?;

        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("failed reading command")?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.split_whitespace();
        let command = parts.next().unwrap_or_default();
        match command {
            "help" => print_help(),
            "quit" | "exit" => break,
            "apps" | "refresh" => {
                apps = fetch_apps(&config).await.unwrap_or_default();
                print_apps(&apps);
            }
            "deps" | "deployments" => {
                let Some(index) = parse_index(parts.next()) else {
                    println!("usage: deployments <app_index>");
                    continue;
                };
                if let Some(app) = apps.get(index) {
                    match fetch_deployments(&config, app.id).await {
                        Ok(items) => print_deployments(app, &items),
                        Err(error) => println!("error: {error}"),
                    }
                } else {
                    println!("invalid app index");
                }
            }
            "domains" => {
                let Some(index) = parse_index(parts.next()) else {
                    println!("usage: domains <app_index>");
                    continue;
                };
                if let Some(app) = apps.get(index) {
                    match fetch_domains(&config, app.id).await {
                        Ok(items) => print_domains(app, &items),
                        Err(error) => println!("error: {error}"),
                    }
                } else {
                    println!("invalid app index");
                }
            }
            "deploy" => {
                let Some(index) = parse_index(parts.next()) else {
                    println!("usage: deploy <app_index> [source_ref]");
                    continue;
                };
                let source_ref = parts.next().unwrap_or("manual").to_string();
                if let Some(app) = apps.get(index) {
                    match trigger_deploy(&config, app.id, source_ref).await {
                        Ok(accepted) => {
                            println!("deployment queued: {}", accepted.deployment_id);
                        }
                        Err(error) => println!("error: {error}"),
                    }
                } else {
                    println!("invalid app index");
                }
            }
            "logs" => {
                let Some(app_index) = parse_index(parts.next()) else {
                    println!("usage: logs <app_index> [deployment_index]");
                    continue;
                };
                let deployment_index = parse_index(parts.next()).unwrap_or(0);
                if let Some(app) = apps.get(app_index) {
                    match fetch_deployments(&config, app.id).await {
                        Ok(deployments) => {
                            if let Some(deployment) = deployments.get(deployment_index) {
                                match fetch_deployment_logs(&config, app.id, deployment.id).await {
                                    Ok(logs) => {
                                        println!(
                                            "logs for deployment {}:\n{}",
                                            logs.deployment_id, logs.logs
                                        );
                                    }
                                    Err(error) => println!("error: {error}"),
                                }
                            } else {
                                println!("invalid deployment index");
                            }
                        }
                        Err(error) => println!("error: {error}"),
                    }
                } else {
                    println!("invalid app index");
                }
            }
            "watch-logs" => {
                let Some(app_index) = parse_index(parts.next()) else {
                    println!("usage: watch-logs <app_index> [deployment_index]");
                    continue;
                };
                let deployment_index = parse_index(parts.next()).unwrap_or(0);
                if let Some(app) = apps.get(app_index) {
                    match fetch_deployments(&config, app.id).await {
                        Ok(deployments) => {
                            if let Some(deployment) = deployments.get(deployment_index) {
                                tail_logs(&config, app.id, deployment.id).await;
                            } else {
                                println!("invalid deployment index");
                            }
                        }
                        Err(error) => println!("error: {error}"),
                    }
                } else {
                    println!("invalid app index");
                }
            }
            "rollback" => {
                let Some(index) = parse_index(parts.next()) else {
                    println!("usage: rollback <app_index>");
                    continue;
                };
                if let Some(app) = apps.get(index) {
                    match trigger_rollback(&config, app.id).await {
                        Ok(accepted) => {
                            println!("rollback queued as deployment: {}", accepted.deployment_id);
                        }
                        Err(error) => println!("error: {error}"),
                    }
                } else {
                    println!("invalid app index");
                }
            }
            "config" => {
                let Some(index) = parse_index(parts.next()) else {
                    println!("usage: config <app_index>");
                    continue;
                };
                if let Some(app) = apps.get(index) {
                    match fetch_effective_config(&config, app.id).await {
                        Ok(effective) => print_effective_config(app, &effective),
                        Err(error) => println!("error: {error}"),
                    }
                } else {
                    println!("invalid app index");
                }
            }
            "import" => {
                let Some(owner) = parts.next() else {
                    println!("usage: import <owner> <repo> <branch> [clone_url]");
                    continue;
                };
                let Some(repo) = parts.next() else {
                    println!("usage: import <owner> <repo> <branch> [clone_url]");
                    continue;
                };
                let Some(branch) = parts.next() else {
                    println!("usage: import <owner> <repo> <branch> [clone_url]");
                    continue;
                };
                let clone_url = parts.next().map(str::to_string);

                match import_app(&config, owner, repo, branch, clone_url).await {
                    Ok(imported) => {
                        println!(
                            "imported app {} ({}) framework={} package_manager={}",
                            imported.app.name,
                            imported.app.id,
                            imported.detection.framework,
                            imported.detection.package_manager
                        );
                        apps = fetch_apps(&config).await.unwrap_or_default();
                        print_apps(&apps);
                    }
                    Err(error) => println!("error: {error}"),
                }
            }
            _ => println!("unknown command: {command}"),
        }
    }

    Ok(())
}

fn parse_index(value: Option<&str>) -> Option<usize> {
    value.and_then(|raw| raw.parse::<usize>().ok())
}

fn print_help() {
    println!("commands:");
    println!("  apps | refresh");
    println!("  import <owner> <repo> <branch> [clone_url]");
    println!("  config <app_index>");
    println!("  deployments <app_index>");
    println!("  domains <app_index>");
    println!("  logs <app_index> [deployment_index]");
    println!("  watch-logs <app_index> [deployment_index]");
    println!("  deploy <app_index> [source_ref]");
    println!("  rollback <app_index>");
    println!("  help");
    println!("  quit");
}

fn print_apps(items: &[AppSummary]) {
    if items.is_empty() {
        println!("no apps");
        return;
    }

    for (index, app) in items.iter().enumerate() {
        println!("[{index}] {} ({})", app.name, app.id);
    }
}

fn print_deployments(app: &AppSummary, items: &[DeploymentSummary]) {
    println!("deployments for {}", app.name);
    if items.is_empty() {
        println!("  none");
        return;
    }

    for deployment in items {
        let source_ref = deployment.source_ref.as_deref().unwrap_or("unknown");
        println!(
            "  {} status={} source={}",
            deployment.id,
            deployment.status.as_str(),
            source_ref
        );
    }
}

fn print_domains(app: &AppSummary, items: &[DomainSummary]) {
    println!("domains for {}", app.name);
    if items.is_empty() {
        println!("  none");
        return;
    }

    for domain in items {
        println!("  {} ({})", domain.domain, domain.tls_mode);
    }
}

fn print_effective_config(app: &AppSummary, config: &EffectiveAppConfigResponse) {
    let branch = config.source.branch.as_deref().unwrap_or("main");
    println!("effective config for {} ({})", app.name, app.id);
    println!(
        "  repo={}/{} branch={}",
        config.repository.owner, config.repository.name, branch
    );
    println!(
        "  build_mode={} framework={} package_manager={}",
        config.build_mode, config.detection.framework, config.detection.package_manager
    );
    if let Some(profile) = &config.dependency_profile {
        println!(
            "  dependencies postgres={:?} redis={:?}",
            profile.postgres, profile.redis
        );
    } else {
        println!("  dependencies none");
    }
}

async fn fetch_health(config: &TuiConfig) -> Result<HealthResponse> {
    let (_, body) = send_request(config, "GET", "/api/v1/health", None).await?;
    serde_json::from_str::<HealthResponse>(&body).context("invalid health response payload")
}

async fn fetch_apps(config: &TuiConfig) -> Result<Vec<AppSummary>> {
    let (status, body) = send_request(config, "GET", "/api/v1/apps", None).await?;
    if status != 200 {
        anyhow::bail!("list apps failed with status {status}");
    }
    let payload: AppListResponse =
        serde_json::from_str(&body).context("invalid apps list payload")?;
    Ok(payload.items)
}

async fn fetch_deployments(config: &TuiConfig, app_id: Uuid) -> Result<Vec<DeploymentSummary>> {
    let (status, body) = send_request(
        config,
        "GET",
        &format!("/api/v1/apps/{app_id}/deployments"),
        None,
    )
    .await?;
    if status != 200 {
        anyhow::bail!("list deployments failed with status {status}");
    }
    let payload: DeploymentListResponse =
        serde_json::from_str(&body).context("invalid deployment list payload")?;
    Ok(payload.items)
}

async fn fetch_domains(config: &TuiConfig, app_id: Uuid) -> Result<Vec<DomainSummary>> {
    let (status, body) = send_request(
        config,
        "GET",
        &format!("/api/v1/apps/{app_id}/domains"),
        None,
    )
    .await?;
    if status != 200 {
        anyhow::bail!("list domains failed with status {status}");
    }
    let payload: DomainListResponse =
        serde_json::from_str(&body).context("invalid domain list payload")?;
    Ok(payload.items)
}

async fn fetch_deployment_logs(
    config: &TuiConfig,
    app_id: Uuid,
    deployment_id: Uuid,
) -> Result<DeploymentLogsResponse> {
    let (status, body) = send_request(
        config,
        "GET",
        &format!("/api/v1/apps/{app_id}/deployments/{deployment_id}/logs"),
        None,
    )
    .await?;
    if status != 200 {
        anyhow::bail!("deployment logs request failed with status {status}");
    }
    serde_json::from_str(&body).context("invalid deployment logs payload")
}

async fn tail_logs(config: &TuiConfig, app_id: Uuid, deployment_id: Uuid) {
    let mut last = String::new();
    for _ in 0..30 {
        match fetch_deployment_logs(config, app_id, deployment_id).await {
            Ok(logs) => {
                if logs.logs != last {
                    println!("--- logs update ---");
                    println!("{}", logs.logs);
                    last = logs.logs;
                }
            }
            Err(error) => {
                println!("error: {error}");
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn fetch_effective_config(
    config: &TuiConfig,
    app_id: Uuid,
) -> Result<EffectiveAppConfigResponse> {
    let (status, body) = send_request(
        config,
        "GET",
        &format!("/api/v1/apps/{app_id}/config"),
        None,
    )
    .await?;
    if status != 200 {
        anyhow::bail!("effective config request failed with status {status}");
    }
    serde_json::from_str(&body).context("invalid effective config payload")
}

async fn import_app(
    config: &TuiConfig,
    owner: &str,
    repo: &str,
    branch: &str,
    clone_url: Option<String>,
) -> Result<ImportAppResponse> {
    let payload = serde_json::to_string(&ImportAppRequest {
        repository: RepositoryRef {
            provider: "github".to_string(),
            owner: owner.to_string(),
            name: repo.to_string(),
            clone_url,
            default_branch: Some(branch.to_string()),
        },
        source: Some(SourceRef {
            branch: Some(branch.to_string()),
            commit_sha: None,
        }),
        build_mode: Some("auto".to_string()),
        runtime: None,
        dependency_profile: None,
    })
    .context("failed to serialize import payload")?;
    let (status, body) = send_request(config, "POST", "/api/v1/apps/import", Some(payload)).await?;
    if status != 201 {
        anyhow::bail!("import failed with status {status}: {body}");
    }
    serde_json::from_str(&body).context("invalid import response payload")
}

async fn trigger_deploy(
    config: &TuiConfig,
    app_id: Uuid,
    source_ref: String,
) -> Result<CreateDeploymentAccepted> {
    let payload = serde_json::to_string(&CreateDeploymentRequest {
        source_ref: Some(source_ref),
        image_ref: None,
        commit_sha: None,
        simulate_failures: Some(0),
        force_rebuild: None,
    })
    .context("failed serializing deploy payload")?;
    let (status, body) = send_request(
        config,
        "POST",
        &format!("/api/v1/apps/{app_id}/deployments"),
        Some(payload),
    )
    .await?;
    if status != 202 {
        anyhow::bail!("deploy failed with status {status}");
    }
    serde_json::from_str(&body).context("invalid deployment accepted payload")
}

async fn trigger_rollback(config: &TuiConfig, app_id: Uuid) -> Result<CreateDeploymentAccepted> {
    let (status, body) = send_request(
        config,
        "POST",
        &format!("/api/v1/apps/{app_id}/rollback"),
        None,
    )
    .await?;
    if status != 202 {
        anyhow::bail!("rollback failed with status {status}");
    }
    serde_json::from_str(&body).context("invalid rollback accepted payload")
}

async fn send_request(
    config: &TuiConfig,
    method: &str,
    path: &str,
    body: Option<String>,
) -> Result<(u16, String)> {
    let body = body.unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n",
        host = config.server_addr
    );
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    if let Some(token) = &config.api_token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);

    let mut stream = tokio::net::TcpStream::connect(&config.server_addr)
        .await
        .with_context(|| format!("failed to connect to {}", config.server_addr))?;
    stream
        .write_all(request.as_bytes())
        .await
        .context("failed to write request")?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .context("failed to read response")?;
    parse_http_response(&response)
}

fn parse_http_response(response: &[u8]) -> Result<(u16, String)> {
    let response = String::from_utf8_lossy(response);
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("missing HTTP status code")?;
    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    Ok((status, body))
}
