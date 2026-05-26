//! Docker admin routes — read-only graph + inspect + snapshots.
//!
//! GET    /admin/docker                       — Cytoscape.js graph page
//! GET    /admin/docker/graph                 — JSON DockerGraph (nodes + edges)
//! GET    /admin/docker/inspect/:id           — JSON ContainerInspectResponse
//! GET    /admin/docker/snapshots             — snapshots page
//! GET    /admin/docker/snapshots/data        — JSON snapshot list
//! POST   /admin/docker/snapshots             — create snapshot { container_id, container_name, image, description }
//! POST   /admin/docker/snapshots/:id/restore — restore snapshot { name }
//! DELETE /admin/docker/snapshots/:id         — delete snapshot (rmi + db)

use crate::{boxed_params, database::Table, models::Account, AppState};
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

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

// ─── Snapshot model ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct DockerSnapshot {
    id: i64,
    container_id: String,
    container_name: String,
    original_image: String,
    snapshot_tag: String,
    description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl Table for DockerSnapshot {
    const NAME: &'static str = "docker_snapshot";
    const COLUMNS: &'static [&'static str] = &[
        "id", "container_id", "container_name", "original_image",
        "snapshot_tag", "description", "created_at",
    ];
    type Id = i64;
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            container_id: row.get("container_id")?,
            container_name: row.get("container_name")?,
            original_image: row.get("original_image")?,
            snapshot_tag: row.get("snapshot_tag")?,
            description: row.get("description")?,
            created_at: row.get("created_at")?,
        })
    }
}

// ─── Snapshots page ───────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin_docker_snapshots.html")]
struct AdminSnapshotsTemplate {
    account: Option<Account>,
    active_page: &'static str,
    docker_available: bool,
}

async fn snapshots_page(
    State(state): State<AppState>,
    account: Account,
) -> Result<AdminSnapshotsTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminSnapshotsTemplate {
        docker_available: state.docker().is_some(),
        account: Some(account),
        active_page: "snapshots",
    })
}

// ─── Snapshots data ───────────────────────────────────────────────────────────

async fn snapshots_data(
    State(state): State<AppState>,
    account: Account,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let snaps: Vec<DockerSnapshot> = match state
        .database()
        .all(
            "SELECT id, container_id, container_name, original_image, snapshot_tag, description, created_at
             FROM docker_snapshot ORDER BY created_at DESC",
            [],
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "snapshots_data query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    Json(serde_json::json!({ "snapshots": snaps })).into_response()
}

// ─── Create snapshot ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateSnapshotPayload {
    container_id: String,
    container_name: String,
    image: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create_snapshot(
    State(state): State<AppState>,
    account: Account,
    Json(payload): Json<CreateSnapshotPayload>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(docker) = state.docker() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({ "error": "Docker not available" }))).into_response();
    };

    let tag = nanoid::nanoid!(12, &nanoid::alphabet::SAFE);
    let snapshot_tag = match docker.commit_snapshot(&payload.container_id, &tag).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "commit_snapshot failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response();
        }
    };

    let desc = payload.description.filter(|s| !s.is_empty());
    match state
        .database()
        .execute(
            "INSERT INTO docker_snapshot (container_id, container_name, original_image, snapshot_tag, description)
             VALUES (?, ?, ?, ?, ?)",
            boxed_params![
                payload.container_id,
                payload.container_name,
                payload.image,
                snapshot_tag.clone(),
                desc
            ],
        )
        .await
    {
        Ok(_) => {
            state.audit("docker.snapshot.create")
                .actor(&account)
                .target(snapshot_tag.clone())
                .fire();
            Json(serde_json::json!({ "snapshot_tag": snapshot_tag })).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "snapshot DB insert failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

// ─── Restore snapshot ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RestorePayload {
    name: String,
}

async fn restore_snapshot(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(payload): Json<RestorePayload>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(docker) = state.docker() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let snap: Option<DockerSnapshot> = match state
        .database()
        .get::<DockerSnapshot, _, _>(
            "SELECT id, container_id, container_name, original_image, snapshot_tag, description, created_at
             FROM docker_snapshot WHERE id = ?",
            boxed_params![id],
        )
        .await
    {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    };

    let Some(snap) = snap else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let name = payload.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "container name required" }))).into_response();
    }

    match docker.run_snapshot(&snap.snapshot_tag, &name).await {
        Ok(container_id) => {
            state.audit("docker.snapshot.restore")
                .actor(&account)
                .target(format!("{} → {}", snap.snapshot_tag, name))
                .fire();
            Json(serde_json::json!({ "container_id": container_id })).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "run_snapshot failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

// ─── Delete snapshot ──────────────────────────────────────────────────────────

async fn delete_snapshot(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Fetch the tag first so we can rmi it
    let tag: Option<String> = match state
        .database()
        .get_row(
            "SELECT snapshot_tag FROM docker_snapshot WHERE id = ?",
            boxed_params![id],
            |row| row.get::<_, String>("snapshot_tag"),
        )
        .await
    {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    };

    let Some(tag) = tag else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Best-effort rmi (image may already be gone)
    if let Some(docker) = state.docker() {
        if let Err(e) = docker.delete_image(&tag).await {
            tracing::warn!(error = %e, tag, "rmi failed — removing DB record anyway");
        }
    }

    match state
        .database()
        .execute("DELETE FROM docker_snapshot WHERE id = ?", boxed_params![id])
        .await
    {
        Ok(_) => {
            state.audit("docker.snapshot.delete")
                .actor(&account)
                .target(tag)
                .fire();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/docker", get(docker_page))
        .route("/admin/docker/graph", get(graph_data))
        .route("/admin/docker/inspect/:id", get(inspect_container))
        .route("/admin/docker/snapshots", get(snapshots_page).post(create_snapshot))
        .route("/admin/docker/snapshots/data", get(snapshots_data))
        .route("/admin/docker/snapshots/:id/restore", post(restore_snapshot))
        .route("/admin/docker/snapshots/:id", delete(delete_snapshot))
}
