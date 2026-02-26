use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use shared::{AgentHeartbeat, HealthResponse, HeartbeatAccepted};

#[derive(Clone, Debug)]
pub struct AppState;

pub fn create_router() -> Router {
    let state = AppState;

    Router::new()
        .route("/health", get(health))
        .route("/api/v1/health", get(health))
        .route("/api/v1/agents/heartbeat", post(agent_heartbeat))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::ok())
}

async fn agent_heartbeat(
    State(_state): State<AppState>,
    Json(_heartbeat): Json<AgentHeartbeat>,
) -> (StatusCode, Json<HeartbeatAccepted>) {
    (StatusCode::ACCEPTED, Json(HeartbeatAccepted::yes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use serde_json::json;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let app = create_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn heartbeat_endpoint_accepts_payload() {
        let app = create_router();
        let body = json!({
            "agent_id": "00000000-0000-0000-0000-000000000000",
            "timestamp_unix_ms": 1000,
            "agent_version": "0.1.0"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/heartbeat")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
}
