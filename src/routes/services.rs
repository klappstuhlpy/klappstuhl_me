//! Admin-only page that displays the live status of configured services.
//!
//! Services are defined in `config.json` under the `services` key.
//! Each entry specifies a name, kind (docker or screen), and an identifier.

use crate::{
    config::ServiceKind,
    filters,
    models::Account,
    AppState,
};
use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::Redirect,
    routing::{get, post},
    Form, Router,
};
use serde::Deserialize;
use std::process::Command;
use time::OffsetDateTime;

/// Runtime status of a single service, derived from a [`crate::config::ServiceConfig`].
pub struct ServiceStatus {
    pub name: String,
    pub running: bool,
    pub started_at: Option<OffsetDateTime>,
}

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesTemplate {
    account: Option<Account>,
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
            ServiceKind::Docker => ServiceStatus {
                name: cfg.name.clone(),
                running: is_docker_running(&cfg.identifier),
                started_at: docker_started_at(&cfg.identifier),
            },
            ServiceKind::Screen => ServiceStatus {
                name: cfg.name.clone(),
                running: is_screen_running(&cfg.identifier),
                started_at: screen_started_at(&cfg.identifier),
            },
        })
        .collect();

    Ok(ServicesTemplate {
        account: Some(account),
        services,
    })
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
                let _ = Command::new("docker").args(["start", &cfg.identifier]).status();
            }
            ("stop", ServiceKind::Docker) => {
                let _ = Command::new("docker").args(["stop", &cfg.identifier]).status();
            }
            ("start", ServiceKind::Screen) => {
                // Screen sessions require a command — we can't start them without knowing the command.
                // Users should configure this via a wrapper script if needed.
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

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/services", get(services_index))
        .route("/services/action", post(service_action))
}
