//! Docker admin routes — combined services dashboard + dependency graph + snapshots.
//!
//! GET    /admin/docker                       — services cards + Cytoscape.js graph
//! POST   /admin/docker/action                — start / stop / restart / pull / recreate
//! GET    /admin/docker/services/data         — JSON live service status + stats
//! GET    /admin/docker/logs/:name            — SSE container log stream
//! GET    /admin/docker/graph                 — JSON DockerGraph (nodes + edges)
//! GET    /admin/docker/inspect/:id           — JSON ContainerInspectResponse
//! GET    /admin/docker/snapshots             — snapshots page
//! GET    /admin/docker/snapshots/data        — JSON snapshot list
//! POST   /admin/docker/snapshots             — create snapshot
//! POST   /admin/docker/snapshots/:id/restore — restore snapshot
//! DELETE /admin/docker/snapshots/:id         — delete snapshot

use crate::{boxed_params, database::Table, filters, headers::ClientIp, metrics::DockerStat, models::Account, AppState};
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json, Redirect, Response,
    },
    routing::{delete, get, post},
    Form, Router,
};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, process::Command};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, BufReader};

// ─── Service status model ─────────────────────────────────────────────────────

/// Runtime status of a single Docker service from [`crate::config::ServiceConfig`].
pub struct ServiceStatus {
    pub name: String,
    pub running: bool,
    pub started_at: Option<OffsetDateTime>,
    pub image: Option<String>,
    pub short_id: Option<String>,
    pub restart_count: Option<u32>,
}

// ─── Docker process helpers ───────────────────────────────────────────────────

fn is_docker_running(identifier: &str) -> bool {
    Command::new("docker")
        .args(["ps", "--filter", &format!("name={identifier}"), "--format", "{{.Names}}"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(identifier))
        .unwrap_or(false)
}

fn docker_started_at(identifier: &str) -> Option<OffsetDateTime> {
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.State.StartedAt}}", identifier])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s.contains("Error") { return None; }
    OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339).ok()
}

/// Returns `(image, short_id, restart_count)` via `docker inspect`.
fn docker_details(identifier: &str) -> (Option<String>, Option<String>, Option<u32>) {
    let out = Command::new("docker")
        .args([
            "inspect", "--format",
            "{{.Config.Image}}\t{{slice .Id 0 12}}\t{{.RestartCount}}",
            identifier,
        ])
        .output()
        .ok();

    let Some(out) = out else { return (None, None, None); };
    let raw = String::from_utf8_lossy(&out.stdout);
    let s = raw.trim();
    if s.is_empty() || s.starts_with("Error") { return (None, None, None); }

    let mut parts = s.splitn(3, '\t');
    let image   = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
    let short_id = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
    let restart_count = parts.next().and_then(|s| s.trim().parse().ok());
    (image, short_id, restart_count)
}

// ─── Kill-on-drop guard ───────────────────────────────────────────────────────

struct KillOnDrop(tokio::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) { let _ = self.0.start_kill(); }
}

// ─── Combined Docker page ─────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin_docker.html")]
struct AdminDockerTemplate {
    account: Option<Account>,
    active_page: &'static str,
    docker_available: bool,
    services: Vec<ServiceStatus>,
}

async fn docker_page(
    State(state): State<AppState>,
    account: Account,
) -> Result<AdminDockerTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let services = state.config().services.iter().map(|cfg| {
        let running   = is_docker_running(&cfg.identifier);
        let started_at = if running { docker_started_at(&cfg.identifier) } else { None };
        let (image, short_id, restart_count) = docker_details(&cfg.identifier);
        ServiceStatus { name: cfg.name.clone(), running, started_at, image, short_id, restart_count }
    }).collect();

    Ok(AdminDockerTemplate {
        docker_available: state.docker().is_some(),
        account: Some(account),
        active_page: "docker",
        services,
    })
}

// ─── Service action (form POST) ───────────────────────────────────────────────

#[derive(Deserialize)]
struct ServiceAction {
    name: String,
    action: String,
}

async fn service_action(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Form(data): Form<ServiceAction>,
) -> Result<Redirect, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let cfg = state.config().services.iter().find(|s| s.name == data.name).cloned();

    if cfg.is_some() {
        state.audit("service.action")
            .actor(&account)
            .target(data.name.clone())
            .ip_opt(client_ip)
            .meta(serde_json::json!({ "action": data.action }))
            .fire();
    }

    if let Some(cfg) = cfg {
        match data.action.as_str() {
            "start" => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker").args(["compose", "up", "-d"]).current_dir(path).status();
                } else {
                    let _ = Command::new("docker").args(["start", &cfg.identifier]).status();
                }
            }
            "stop" => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker").args(["compose", "down"]).current_dir(path).status();
                } else {
                    let _ = Command::new("docker").args(["stop", &cfg.identifier]).status();
                }
            }
            "restart" => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker").args(["compose", "restart"]).current_dir(path).status();
                } else {
                    let _ = Command::new("docker").args(["restart", &cfg.identifier]).status();
                }
            }
            "pull" => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker").args(["compose", "pull"]).current_dir(path).status();
                } else {
                    let image_out = Command::new("docker")
                        .args(["inspect", "-f", "{{.Config.Image}}", &cfg.identifier])
                        .output();
                    if let Ok(out) = image_out {
                        let image = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                        if !image.is_empty() && !image.starts_with("Error") {
                            let _ = Command::new("docker").args(["pull", &image]).status();
                        }
                    }
                }
            }
            "recreate" => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker")
                        .args(["compose", "up", "-d", "--force-recreate"])
                        .current_dir(path).status();
                } else {
                    let _ = Command::new("docker").args(["stop", &cfg.identifier]).status();
                    let _ = Command::new("docker").args(["start", &cfg.identifier]).status();
                }
            }
            _ => {}
        }
    }

    Ok(Redirect::to("/admin/docker"))
}

// ─── Live service data (JSON) ─────────────────────────────────────────────────

#[derive(Serialize)]
struct ServiceView {
    name: String,
    running: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    image: Option<String>,
    short_id: Option<String>,
    restart_count: Option<u32>,
    cpu_pct: Option<f64>,
    mem_used: Option<u64>,
    mem_limit: Option<u64>,
}

async fn services_data(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<Vec<ServiceView>>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let stats: Vec<DockerStat> = crate::metrics::docker::collect().await.unwrap_or_default();
    let stats_by_name: std::collections::HashMap<&str, &DockerStat> =
        stats.iter().map(|s| (s.name.as_str(), s)).collect();

    let views = state.config().services.iter().map(|cfg| {
        let running   = is_docker_running(&cfg.identifier);
        let started_at = if running { docker_started_at(&cfg.identifier) } else { None };
        let (image, short_id, restart_count) = docker_details(&cfg.identifier);
        let stat = stats_by_name.get(cfg.identifier.as_str()).copied();

        ServiceView {
            name: cfg.name.clone(),
            running,
            started_at,
            image,
            short_id,
            restart_count,
            cpu_pct:   stat.map(|s| s.cpu_pct),
            mem_used:  stat.map(|s| s.mem_used),
            mem_limit: stat.map(|s| s.mem_limit),
        }
    }).collect();

    Ok(Json(views))
}

// ─── Container log SSE ────────────────────────────────────────────────────────

async fn container_logs_sse(
    State(state): State<AppState>,
    account: Account,
    Path(name): Path<String>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let cfg = state
        .config()
        .services
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    type LogStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Event, Infallible>> + Send>>;

    let mut child = tokio::process::Command::new("docker")
        .args(["logs", "--follow", "--tail", "200", &cfg.identifier])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stdout = child.stdout.take().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let stderr = child.stderr.take().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let (tx, rx) = tokio::sync::mpsc::channel::<String>(256);
    let tx2 = tx.clone();

    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).await.is_err() { break; }
        }
    });

    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx2.send(line).await.is_err() { break; }
        }
    });

    let log_stream: LogStream = Box::pin(stream::unfold(
        (rx, KillOnDrop(child)),
        |(mut rx, killer)| async move {
            rx.recv().await.map(|line| {
                (Ok::<_, Infallible>(Event::default().data(line)), (rx, killer))
            })
        },
    ));

    Ok(Sse::new(log_stream).keep_alive(KeepAlive::default()))
}

// ─── Graph data ───────────────────────────────────────────────────────────────

async fn graph_data(State(state): State<AppState>, account: Account) -> Response {
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
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
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
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
    }
}

// ─── Snapshot model ───────────────────────────────────────────────────────────

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

async fn snapshots_data(State(state): State<AppState>, account: Account) -> Response {
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
            boxed_params![payload.container_id, payload.container_name, payload.image, snapshot_tag.clone(), desc],
        )
        .await
    {
        Ok(_) => {
            state.audit("docker.snapshot.create").actor(&account).target(snapshot_tag.clone()).fire();
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
struct RestorePayload { name: String }

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

    if let Some(docker) = state.docker() {
        if let Err(e) = docker.delete_image(&tag).await {
            tracing::warn!(error = %e, tag, "rmi failed — removing DB record anyway");
        }
    }

    match state.database().execute("DELETE FROM docker_snapshot WHERE id = ?", boxed_params![id]).await {
        Ok(_) => {
            state.audit("docker.snapshot.delete").actor(&account).target(tag).fire();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/docker",                          get(docker_page))
        .route("/admin/docker/action",                   post(service_action))
        .route("/admin/docker/services/data",            get(services_data))
        .route("/admin/docker/logs/:name",               get(container_logs_sse))
        .route("/admin/docker/graph",                    get(graph_data))
        .route("/admin/docker/inspect/:id",              get(inspect_container))
        .route("/admin/docker/snapshots",                get(snapshots_page).post(create_snapshot))
        .route("/admin/docker/snapshots/data",           get(snapshots_data))
        .route("/admin/docker/snapshots/:id/restore",    post(restore_snapshot))
        .route("/admin/docker/snapshots/:id",            delete(delete_snapshot))
}
