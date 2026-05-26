//! Docker admin routes — read-only graph + inspect.
//!
//! GET /admin/docker          — Cytoscape.js graph page
//! GET /admin/docker/graph    — JSON DockerGraph (nodes + edges)
//! GET /admin/docker/inspect/:id — JSON ContainerInspectResponse

use crate::{models::Account, AppState};
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};

// ─── Page ─────────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin_docker.html")]
struct AdminDockerTemplate {
    account: Option<Account>,
    active_page: &'static str,
    docker_available: bool,
}

async fn docker_page(
    State(state): State<AppState>,
    account: Account,
) -> Result<AdminDockerTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminDockerTemplate {
        docker_available: state.docker().is_some(),
        account: Some(account),
        active_page: "docker",
    })
}

// ─── Graph data ───────────────────────────────────────────────────────────────

async fn graph_data(
    State(state): State<AppState>,
    account: Account,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(docker) = state.docker() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
            "error": "Docker socket not available"
        }))).into_response();
    };
    match docker.build_graph().await {
        Ok(graph) => Json(graph).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "build_graph failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": e.to_string()
            }))).into_response()
        }
    }
}

// ─── Container inspect ────────────────────────────────────────────────────────

async fn inspect_container(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(docker) = state.docker() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match docker.inspect(&id).await {
        Ok(info) => Json(info).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, id, "inspect_container failed");
            (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": e.to_string()
            }))).into_response()
        }
    }
}

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/docker", get(docker_page))
        .route("/admin/docker/graph", get(graph_data))
        .route("/admin/docker/inspect/:id", get(inspect_container))
}
