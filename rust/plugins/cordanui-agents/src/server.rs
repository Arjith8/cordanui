//! HTTP server using axum. Exposes `/run` and `/health` endpoints.

use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::executor::Executor;

/// Shared server state.
#[derive(Clone)]
pub struct AppState {
    pub executor: Arc<Executor>,
    pub auth_token: Option<String>,
}

/// Request body for POST /run.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub task_id: String,
}

/// Response body for POST /run.
#[derive(Debug, Serialize)]
pub struct RunResponse {
    pub task_id: String,
    pub accepted: bool,
    pub message: String,
}

/// Response body for GET /health.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Start the HTTP server.
pub async fn serve(config: Config) -> Result<()> {
    let port = config.port;

    // Open the database
    let db = crate::db::open(config.db_path.as_deref())?;
    let executor = Arc::new(Executor::new(config.clone(), db));

    let state = AppState {
        executor,
        auth_token: config.auth_token.clone(),
    };

    let app = Router::new()
        .route("/run", post(handle_run))
        .route("/health", get(handle_health))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    info!(addr = %addr, "starting agent backend");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Check the Authorization header against the configured auth token.
fn check_auth(headers: &HeaderMap, auth_token: &Option<String>) -> bool {
    match auth_token {
        None => true,
        Some(token) => {
            if let Some(auth_header) = headers.get("authorization") {
                if let Ok(auth_str) = auth_header.to_str() {
                    if let Some(bearer) = auth_str.strip_prefix("Bearer ") {
                        return bearer == token;
                    }
                }
            }
            false
        }
    }
}

async fn handle_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Result<Json<RunResponse>, (StatusCode, String)> {
    if !check_auth(&headers, &state.auth_token) {
        return Err((StatusCode::UNAUTHORIZED, "unauthorized".to_string()));
    }

    let task_id = req.task_id.clone();
    info!(task_id = %task_id, "received run request");

    let executor = state.executor.clone();
    let task_id_for_task = task_id.clone();

    tokio::spawn(async move {
        match executor.execute(&task_id_for_task).await {
            Ok(result) => {
                if result.success {
                    info!(task_id = %result.task_id, "task completed");
                } else {
                    warn!(task_id = %result.task_id, "task failed: {}", result.message);
                }
            }
            Err(e) => {
                error!(task_id = %task_id_for_task, "execution error: {e}");
            }
        }
    });

    Ok(Json(RunResponse {
        task_id,
        accepted: true,
        message: "task accepted, execution started".to_string(),
    }))
}

async fn handle_health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
