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

use crate::{
    boxed_params, database::Table, filters, headers::ClientIp, metrics::DockerStat, models::Account, AppState,
};
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json, Response,
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
        .args([
            "ps",
            "--filter",
            &format!("name={identifier}"),
            "--format",
            "{{.Names}}",
        ])
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
    if s.is_empty() || s.contains("Error") {
        return None;
    }
    OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339).ok()
}

/// Returns `(image, short_id, restart_count)` via `docker inspect`.
fn docker_details(identifier: &str) -> (Option<String>, Option<String>, Option<u32>) {
    let out = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.Config.Image}}\t{{slice .Id 0 12}}\t{{.RestartCount}}",
            identifier,
        ])
        .output()
        .ok();

    let Some(out) = out else {
        return (None, None, None);
    };
    let raw = String::from_utf8_lossy(&out.stdout);
    let s = raw.trim();
    if s.is_empty() || s.starts_with("Error") {
        return (None, None, None);
    }

    let mut parts = s.splitn(3, '\t');
    let image = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
    let short_id = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
    let restart_count = parts.next().and_then(|s| s.trim().parse().ok());
    (image, short_id, restart_count)
}

// ─── Kill-on-drop guard ───────────────────────────────────────────────────────

struct KillOnDrop(tokio::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// ─── Docker action log ────────────────────────────────────────────────────────
//
// An in-memory ring buffer of the most recent service actions (start / stop /
// restart / pull / recreate) together with the captured command output. This
// is what powers the "Action Log" panel on the Docker admin page so the
// operator can actually see what happened when they click a button — the old
// flow inherited stdout to the server console and showed the user nothing.

/// One recorded service action and its captured command output.
#[derive(Clone, Serialize)]
struct DockerActionLog {
    #[serde(with = "time::serde::rfc3339")]
    ts: OffsetDateTime,
    service: String,
    action: String,
    success: bool,
    actor: String,
    output: String,
}

/// Maximum number of action-log entries kept in memory.
const ACTION_LOG_CAP: usize = 200;

/// Process-wide action-log ring buffer (newest entry at the front).
fn action_log() -> &'static std::sync::Mutex<std::collections::VecDeque<DockerActionLog>> {
    static LOG: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<DockerActionLog>>> =
        std::sync::OnceLock::new();
    LOG.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
}

fn record_action(entry: DockerActionLog) {
    if let Ok(mut log) = action_log().lock() {
        log.push_front(entry);
        log.truncate(ACTION_LOG_CAP);
    }
}

/// Run `docker <args>` (optionally in `cwd`), capturing combined stdout+stderr.
/// Returns `(success, trimmed_output)`. Docker writes pull/compose progress to
/// stderr, so both streams are merged into the returned text.
async fn run_docker(args: &[&str], cwd: Option<&str>) -> (bool, String) {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    match cmd.output().await {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&stderr);
            }
            (out.status.success(), text.trim_end().to_string())
        }
        Err(e) => (
            false,
            format!("$ docker {}\nfailed to launch docker: {e}", args.join(" ")),
        ),
    }
}

/// Perform one service action, returning `(success, captured_output)`.
///
/// When the service has a compose `path` we drive `docker compose`; otherwise
/// we operate on the bare container by `identifier`.
async fn perform_action(path: Option<&str>, identifier: &str, action: &str) -> (bool, String) {
    match action {
        "start" => match path {
            Some(p) => run_docker(&["compose", "up", "-d"], Some(p)).await,
            None => run_docker(&["start", identifier], None).await,
        },
        "stop" => match path {
            Some(p) => run_docker(&["compose", "down"], Some(p)).await,
            None => run_docker(&["stop", identifier], None).await,
        },
        "restart" => match path {
            Some(p) => run_docker(&["compose", "restart"], Some(p)).await,
            None => run_docker(&["restart", identifier], None).await,
        },
        "pull" => match path {
            Some(p) => run_docker(&["compose", "pull"], Some(p)).await,
            None => {
                let (_, raw) = run_docker(&["inspect", "-f", "{{.Config.Image}}", identifier], None).await;
                let image = raw.trim();
                if image.is_empty() || image.starts_with("Error") {
                    (false, "could not determine the image to pull".to_string())
                } else {
                    run_docker(&["pull", image], None).await
                }
            }
        },
        "recreate" => match path {
            Some(p) => run_docker(&["compose", "up", "-d", "--force-recreate"], Some(p)).await,
            None => {
                // No compose file to recreate from — the best we can do for a
                // bare container is stop + start it. Capture both steps.
                let (s1, o1) = run_docker(&["stop", identifier], None).await;
                let (s2, o2) = run_docker(&["start", identifier], None).await;
                (s1 && s2, format!("{o1}\n{o2}").trim().to_string())
            }
        },
        other => (false, format!("unknown action: {other}")),
    }
}

// ─── Combined Docker page ─────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin/admin_docker.html")]
struct AdminDockerTemplate {
    account: Option<Account>,
    active_page: &'static str,
    docker_available: bool,
    services: Vec<ServiceStatus>,
}

async fn docker_page(State(state): State<AppState>, account: Account) -> Result<AdminDockerTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let services = state
        .config()
        .services
        .iter()
        .map(|cfg| {
            let running = is_docker_running(&cfg.identifier);
            let started_at = if running {
                docker_started_at(&cfg.identifier)
            } else {
                None
            };
            let (image, short_id, restart_count) = docker_details(&cfg.identifier);
            ServiceStatus {
                name: cfg.name.clone(),
                running,
                started_at,
                image,
                short_id,
                restart_count,
            }
        })
        .collect();

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
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let Some(cfg) = state.config().services.iter().find(|s| s.name == data.name).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown service" })),
        )
            .into_response();
    };

    if !matches!(data.action.as_str(), "start" | "stop" | "restart" | "pull" | "recreate") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "unknown action" })),
        )
            .into_response();
    }

    state
        .audit("service.action")
        .actor(&account)
        .target(data.name.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "action": data.action }))
        .fire();

    let (success, output) = perform_action(cfg.path.as_deref(), &cfg.identifier, &data.action).await;

    // State changed — drop the cached container/network/volume lists so the
    // graph and live data reflect the new reality immediately.
    if let Some(docker) = state.docker() {
        docker.invalidate().await;
    }

    record_action(DockerActionLog {
        ts: OffsetDateTime::now_utc(),
        service: cfg.name.clone(),
        action: data.action.clone(),
        success,
        actor: account.name.clone(),
        output: output.clone(),
    });

    let status = if success {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (
        status,
        Json(serde_json::json!({
            "ok": success,
            "service": cfg.name,
            "action": data.action,
            "output": output,
        })),
    )
        .into_response()
}

// ─── Action log data (JSON) ───────────────────────────────────────────────────

async fn action_log_data(account: Account) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let entries: Vec<DockerActionLog> = action_log()
        .lock()
        .map(|g| g.iter().cloned().collect())
        .unwrap_or_default();
    Json(serde_json::json!({ "actions": entries })).into_response()
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
    /// Latest image-update status from the background checker, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<crate::updates::ImageUpdate>,
}

async fn services_data(State(state): State<AppState>, account: Account) -> Result<Json<Vec<ServiceView>>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let stats: Vec<DockerStat> = crate::metrics::docker::collect().await.unwrap_or_default();
    let stats_by_name: std::collections::HashMap<&str, &DockerStat> =
        stats.iter().map(|s| (s.name.as_str(), s)).collect();

    let views = state
        .config()
        .services
        .iter()
        .map(|cfg| {
            let running = is_docker_running(&cfg.identifier);
            let started_at = if running {
                docker_started_at(&cfg.identifier)
            } else {
                None
            };
            let (image, short_id, restart_count) = docker_details(&cfg.identifier);
            let stat = stats_by_name.get(cfg.identifier.as_str()).copied();

            ServiceView {
                name: cfg.name.clone(),
                running,
                started_at,
                image,
                short_id,
                restart_count,
                cpu_pct: stat.map(|s| s.cpu_pct),
                mem_used: stat.map(|s| s.mem_used),
                mem_limit: stat.map(|s| s.mem_limit),
                update: state.image_update(&cfg.name),
            }
        })
        .collect();

    Ok(Json(views))
}

/// Runs an image-update check across all services on demand and returns the
/// fresh results. Synchronous — a homelab has a handful of services, and the
/// operator clicked "Check" expecting an answer.
async fn check_updates_now(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    crate::updates::run_check(&state).await;
    state
        .audit("docker.updates.check")
        .actor(&account)
        .ip_opt(client_ip)
        .fire();
    let mut updates: Vec<crate::updates::ImageUpdate> = state.image_updates_map().into_values().collect();
    updates.sort_by(|a, b| a.service.cmp(&b.service));
    Json(serde_json::json!({ "updates": updates })).into_response()
}

// ─── Container log SSE ────────────────────────────────────────────────────────

async fn container_logs_sse(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
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

    // Audit once per stream-open. Container logs can include secrets and
    // request bodies, so privileged read.
    state
        .audit("docker.container.logs.open")
        .actor(&account)
        .target(format!("service:{name} → {}", cfg.identifier))
        .ip_opt(client_ip)
        .fire();

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
            if tx.send(line).await.is_err() {
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx2.send(line).await.is_err() {
                break;
            }
        }
    });

    let log_stream: LogStream = Box::pin(stream::unfold((rx, KillOnDrop(child)), |(mut rx, killer)| async move {
        rx.recv()
            .await
            .map(|line| (Ok::<_, Infallible>(Event::default().data(line)), (rx, killer)))
    }));

    Ok(Sse::new(log_stream).keep_alive(KeepAlive::default()))
}

// ─── Graph data ───────────────────────────────────────────────────────────────

async fn graph_data(State(state): State<AppState>, account: Account) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(docker) = state.docker() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "Docker socket not available"
            })),
        )
            .into_response();
    };
    match docker.build_graph().await {
        Ok(graph) => Json(graph).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "build_graph failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

// ─── Container inspect ────────────────────────────────────────────────────────

async fn inspect_container(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
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
        Ok(info) => {
            // Inspect dumps env vars, mounts, and command line — those
            // routinely contain credentials. Audit reads.
            state
                .audit("docker.container.inspect")
                .actor(&account)
                .target(id.clone())
                .ip_opt(client_ip)
                .fire();
            Json(info).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, id, "inspect_container failed");
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
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
        "id",
        "container_id",
        "container_name",
        "original_image",
        "snapshot_tag",
        "description",
        "created_at",
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
#[template(path = "admin/admin_docker_snapshots.html")]
struct AdminSnapshotsTemplate {
    account: Option<Account>,
    active_page: &'static str,
    docker_available: bool,
}

async fn snapshots_page(State(state): State<AppState>, account: Account) -> Result<AdminSnapshotsTemplate, StatusCode> {
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
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Docker not available" })),
        )
            .into_response();
    };

    let tag = nanoid::nanoid!(12, &nanoid::alphabet::SAFE);
    let snapshot_tag = match docker.commit_snapshot(&payload.container_id, &tag).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "commit_snapshot failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
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
            state
                .audit("docker.snapshot.create")
                .actor(&account)
                .target(snapshot_tag.clone())
                .fire();
            Json(serde_json::json!({ "snapshot_tag": snapshot_tag })).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "snapshot DB insert failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
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
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };

    let Some(snap) = snap else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let name = payload.name.trim().to_owned();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "container name required" })),
        )
            .into_response();
    }

    match docker.run_snapshot(&snap.snapshot_tag, &name).await {
        Ok(container_id) => {
            state
                .audit("docker.snapshot.restore")
                .actor(&account)
                .target(format!("{} → {}", snap.snapshot_tag, name))
                .fire();
            Json(serde_json::json!({ "container_id": container_id })).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "run_snapshot failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

// ─── Delete snapshot ──────────────────────────────────────────────────────────

async fn delete_snapshot(State(state): State<AppState>, account: Account, Path(id): Path<i64>) -> Response {
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
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };

    let Some(tag) = tag else {
        return StatusCode::NOT_FOUND.into_response();
    };

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
            state.audit("docker.snapshot.delete").actor(&account).target(tag).fire();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/docker", get(docker_page))
        .route("/admin/docker/action", post(service_action))
        .route("/admin/docker/actions/log", get(action_log_data))
        .route("/admin/docker/services/data", get(services_data))
        .route("/admin/docker/updates/check", post(check_updates_now))
        .route("/admin/docker/logs/:name", get(container_logs_sse))
        .route("/admin/docker/graph", get(graph_data))
        .route("/admin/docker/inspect/:id", get(inspect_container))
        .route("/admin/docker/snapshots", get(snapshots_page).post(create_snapshot))
        .route("/admin/docker/snapshots/data", get(snapshots_data))
        .route("/admin/docker/snapshots/:id/restore", post(restore_snapshot))
        .route("/admin/docker/snapshots/:id", delete(delete_snapshot))
}
