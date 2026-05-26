//! Admin-only page that displays the live status of configured services.
//!
//! Services are defined in `config.json` under the `services` key.
//! Each entry specifies a name, kind (docker or screen), and an identifier.

use crate::{
    config::ServiceKind,
    filters,
    metrics::DockerStat,
    models::Account,
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Json, Redirect,
    },
    routing::{get, post},
    Form, Router,
};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, process::Command};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Runtime status of a single service, derived from a [`crate::config::ServiceConfig`].
pub struct ServiceStatus {
    pub name: String,
    pub kind: ServiceKind,
    pub running: bool,
    pub started_at: Option<OffsetDateTime>,
    /// Docker image name (Docker only).
    pub image: Option<String>,
    /// First 12 characters of the container ID (Docker only).
    pub short_id: Option<String>,
    /// Total automatic restart count (Docker only).
    pub restart_count: Option<u32>,
}

impl ServiceStatus {
    /// Human-readable kind label for use in templates.
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            ServiceKind::Docker => "Docker",
            ServiceKind::Screen => "Screen",
        }
    }

    /// Templates use this instead of a path-qualified match arm.
    pub fn is_docker(&self) -> bool {
        matches!(self.kind, ServiceKind::Docker)
    }
}

/// Wraps a child process and kills it when dropped.
///
/// Used to ensure `docker logs --follow` exits when the SSE client disconnects.
struct KillOnDrop(tokio::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

#[derive(Template)]
#[template(path = "admin_services.html")]
struct ServicesTemplate {
    account: Option<Account>,
    active_page: &'static str,
    services: Vec<ServiceStatus>,
}

#[derive(Deserialize)]
struct ServiceAction {
    name: String,
    action: String,
}

// --- Docker helpers ---------------------------------------------------------

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
    if s.is_empty() || s.contains("Error") {
        return None;
    }
    // Docker returns RFC 3339 — parse with time
    OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339).ok()
}

/// Returns `(image, short_id, restart_count)` for a Docker container via `docker inspect`.
///
/// Returns `(None, None, None)` if the container does not exist or inspect fails.
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

// --- Screen helpers ---------------------------------------------------------

fn is_screen_running(identifier: &str) -> bool {
    Command::new("screen")
        .args(["-ls", identifier])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(identifier))
        .unwrap_or(false)
}

fn screen_started_at(identifier: &str) -> Option<OffsetDateTime> {
    let pid_out = Command::new("pgrep")
        .args(["-f", identifier])
        .output()
        .ok()?;
    let pid = String::from_utf8_lossy(&pid_out.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();

    let start_out = Command::new("ps")
        .args(["-p", &pid, "-o", "lstart="])
        .output()
        .ok()?;
    let start_str = String::from_utf8_lossy(&start_out.stdout).trim().to_string();
    if start_str.is_empty() {
        return None;
    }
    // ps lstart format: "Fri Oct  3 09:12:34 2025"
    let format = time::macros::format_description!("[weekday repr:short] [month repr:short] [day padding:space] [hour]:[minute]:[second] [year]");
    time::PrimitiveDateTime::parse(&start_str, format)
        .ok()
        .map(|dt| dt.assume_utc())
}

// --- Route handlers ---------------------------------------------------------

async fn services_index(
    State(state): State<AppState>,
    account: Account,
) -> Result<ServicesTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let services = state
        .config()
        .services
        .iter()
        .map(|cfg| match cfg.kind {
            ServiceKind::Docker => {
                let running = is_docker_running(&cfg.identifier);
                let started_at = if running { docker_started_at(&cfg.identifier) } else { None };
                let (image, short_id, restart_count) = docker_details(&cfg.identifier);
                ServiceStatus {
                    name: cfg.name.clone(),
                    kind: cfg.kind.clone(),
                    running,
                    started_at,
                    image,
                    short_id,
                    restart_count,
                }
            }
            ServiceKind::Screen => ServiceStatus {
                name: cfg.name.clone(),
                kind: cfg.kind.clone(),
                running: is_screen_running(&cfg.identifier),
                started_at: screen_started_at(&cfg.identifier),
                image: None,
                short_id: None,
                restart_count: None,
            },
        })
        .collect();

    Ok(ServicesTemplate { account: Some(account), active_page: "services", services })
}

async fn service_action(
    State(state): State<AppState>,
    account: Account,
    Form(data): Form<ServiceAction>,
) -> Result<Redirect, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let cfg = state
        .config()
        .services
        .iter()
        .find(|s| s.name == data.name)
        .cloned();

    if let Some(cfg) = cfg {
        match (data.action.as_str(), &cfg.kind) {
            ("start", ServiceKind::Docker) => {
                if let Some(ref path) = cfg.path {
                    // Run detached so the HTTP handler is not blocked
                    let _ = Command::new("docker")
                        .args(["compose", "up", "-d"])
                        .current_dir(path)
                        .status();
                } else {
                    let _ = Command::new("docker").args(["start", &cfg.identifier]).status();
                }
            }
            ("stop", ServiceKind::Docker) => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker")
                        .args(["compose", "down"])
                        .current_dir(path)
                        .status();
                } else {
                    let _ = Command::new("docker").args(["stop", &cfg.identifier]).status();
                }
            }
            ("restart", ServiceKind::Docker) => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker")
                        .args(["compose", "restart"])
                        .current_dir(path)
                        .status();
                } else {
                    let _ = Command::new("docker").args(["restart", &cfg.identifier]).status();
                }
            }
            // Pull the latest image. For compose stacks, this updates every
            // service defined in the file; for plain containers, just the
            // single image they were created from.
            ("pull", ServiceKind::Docker) => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker")
                        .args(["compose", "pull"])
                        .current_dir(path)
                        .status();
                } else {
                    // Look up the image name from the container, then pull it.
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
            // Force-recreate: stop, remove, start fresh. For compose this
            // is `docker compose up -d --force-recreate`. For a plain
            // container we don't have its full run-config so we fall back
            // to stop + start, which only works if the container itself
            // still exists.
            ("recreate", ServiceKind::Docker) => {
                if let Some(ref path) = cfg.path {
                    let _ = Command::new("docker")
                        .args(["compose", "up", "-d", "--force-recreate"])
                        .current_dir(path)
                        .status();
                } else {
                    let _ = Command::new("docker").args(["stop", &cfg.identifier]).status();
                    let _ = Command::new("docker").args(["start", &cfg.identifier]).status();
                }
            }
            ("start", ServiceKind::Screen) => {
                tracing::warn!("start action for screen sessions is not supported via the web UI");
            }
            ("stop", ServiceKind::Screen) => {
                let _ = Command::new("screen")
                    .args(["-S", &cfg.identifier, "-X", "quit"])
                    .status();
            }
            _ => {}
        }
    }

    Ok(Redirect::to("/services"))
}

/// Streams container logs as Server-Sent Events.
///
/// For Docker services this tails the last 200 lines and then follows new output,
/// merging both the container's stdout and stderr.  GNU Screen sessions are not
/// supported; a single informational message is sent instead.
///
/// The spawned `docker logs --follow` process is killed automatically when the
/// client disconnects (via [`KillOnDrop`]).
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

    // Use a boxed stream so both match arms share the same concrete type.
    type LogStream =
        std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Event, Infallible>> + Send>>;

    let log_stream: LogStream = match cfg.kind {
        ServiceKind::Screen => {
            let s = stream::once(async {
                Ok::<_, Infallible>(
                    Event::default()
                        .data("[Log streaming is not supported for GNU Screen sessions]"),
                )
            });
            Box::pin(s)
        }
        ServiceKind::Docker => {
            let mut child = tokio::process::Command::new("docker")
                .args(["logs", "--follow", "--tail", "200", &cfg.identifier])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            let stdout = child.stdout.take().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
            let stderr = child.stderr.take().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

            // Use a channel to merge stdout + stderr from the spawned reader tasks.
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

            // Dropping `rx` closes the channel → sender tasks exit.
            // Dropping `KillOnDrop` kills the docker process.
            // Both happen automatically when the SSE client disconnects.
            let s = stream::unfold(
                (rx, KillOnDrop(child)),
                |(mut rx, killer)| async move {
                    rx.recv().await.map(|line| {
                        (Ok::<_, Infallible>(Event::default().data(line)), (rx, killer))
                    })
                },
            );
            Box::pin(s)
        }
    };

    Ok(Sse::new(log_stream).keep_alive(KeepAlive::default()))
}

// --- JSON: live service status + per-container stats ----------------------

/// Shape returned by `GET /services/data`. Powers the auto-refresh loop in
/// the dashboard JS so we can update each card without a full page reload.
#[derive(Serialize)]
struct ServiceView {
    name: String,
    kind: String,
    running: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    image: Option<String>,
    short_id: Option<String>,
    restart_count: Option<u32>,
    /// Live CPU percent from `docker stats` (None if stats unavailable
    /// or service is a Screen session).
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

    // One `docker stats` invocation covers every container at once;
    // looking up per-service afterwards is just a hashmap probe.
    let stats: Vec<DockerStat> = crate::metrics::docker::collect().await.unwrap_or_default();
    let stats_by_name: std::collections::HashMap<&str, &DockerStat> =
        stats.iter().map(|s| (s.name.as_str(), s)).collect();

    let views = state
        .config()
        .services
        .iter()
        .map(|cfg| {
            let (running, started_at, image, short_id, restart_count) = match cfg.kind {
                ServiceKind::Docker => {
                    let running = is_docker_running(&cfg.identifier);
                    let started_at = if running { docker_started_at(&cfg.identifier) } else { None };
                    let (image, short_id, restart_count) = docker_details(&cfg.identifier);
                    (running, started_at, image, short_id, restart_count)
                }
                ServiceKind::Screen => (
                    is_screen_running(&cfg.identifier),
                    screen_started_at(&cfg.identifier),
                    None,
                    None,
                    None,
                ),
            };

            // `docker stats` keys by container name, which matches our
            // ServiceConfig.identifier for Docker services.
            let stat = if matches!(cfg.kind, ServiceKind::Docker) {
                stats_by_name.get(cfg.identifier.as_str()).copied()
            } else {
                None
            };

            ServiceView {
                name: cfg.name.clone(),
                kind: match cfg.kind {
                    ServiceKind::Docker => "Docker".into(),
                    ServiceKind::Screen => "Screen".into(),
                },
                running,
                started_at,
                image,
                short_id,
                restart_count,
                cpu_pct: stat.map(|s| s.cpu_pct),
                mem_used: stat.map(|s| s.mem_used),
                mem_limit: stat.map(|s| s.mem_limit),
            }
        })
        .collect();

    Ok(Json(views))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/services", get(services_index))
        .route("/admin/services/action", post(service_action))
        .route("/admin/services/data", get(services_data))
        .route("/admin/services/logs/:name", get(container_logs_sse))
}
