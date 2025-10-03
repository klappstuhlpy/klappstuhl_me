use askama::Template;
use axum::{
    http::StatusCode,
    routing::get,
    routing::post,
    Router,
};
use axum::{Form};
use serde::Deserialize;
use utoipa::ToSchema;
use std::process::Command;
use axum::response::Redirect;
use crate::{
    models::Account,
    AppState,
};
use chrono::{Utc, DateTime};

#[derive(Debug, PartialEq, Eq, Clone, ToSchema)]
pub struct ServiceEntry {
    /// The service's name.
    pub(crate) name: String,
    /// Whether the service is running.
    pub(crate) running: bool,
    /// The start time of the service, if available.
    pub(crate) started_at: Option<DateTime<Utc>>,
}

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesTemplate {
    account: Option<Account>,
    services: Vec<ServiceEntry>,
}

#[derive(Deserialize)]
struct ServiceAction {
    /// The service's name.
    name: String,
    /// The action to perform: "start", "stop".
    action: String,
}

fn docker_container_started_at(name: &str) -> Option<DateTime<Utc>> {
    let output = Command::new("docker")
        .args(["inspect", "-f", "{{.State.StartedAt}}", name])
        .output()
        .ok()?;

    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() || s.contains("Error") {
        return None;
    }

    // parse RFC3339 string into DateTime<Utc>
    DateTime::parse_from_rfc3339(&s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn screen_started_at(name: &str) -> Option<DateTime<Utc>> {
    // 1. get PID
    let pid_output = Command::new("pgrep")
        .args(["-f", name])
        .output()
        .ok()?;
    let pid = String::from_utf8_lossy(&pid_output.stdout)
        .lines()
        .next()? // take first PID
        .trim()
        .to_string();

    // 2. get start time
    let start_output = Command::new("ps")
        .args(["-p", &pid, "-o", "lstart="])
        .output()
        .ok()?;

    let start_str = String::from_utf8_lossy(&start_output.stdout).trim().to_string();
    if start_str.is_empty() {
        return None;
    }

    // 3. optionally convert to chrono datetime
    // Example format: "Fri Oct  3 09:12:34 2025"
    let dt = chrono::NaiveDateTime::parse_from_str(&start_str, "%a %b %e %H:%M:%S %Y").ok()?;

    Some(DateTime::from_utc(dt, Utc))
}

fn get_servicestatus() -> Vec<ServiceEntry> {
    return vec![
        ServiceEntry {
            name: "Lavalink".to_string(),
            running: is_screen_running("lavalink"),
            started_at: screen_started_at("lavalink"),
        },
        ServiceEntry {
            name: "Snekbox API".to_string(),
            running: is_docker_container_running("percy-snekbox"),
            started_at: docker_container_started_at("percy-snekbox"),
        },
        ServiceEntry {
            name: "Database".to_string(),
            running: is_docker_container_running("percy-db"),
            started_at: docker_container_started_at("percy-db"),
        },
        ServiceEntry {
            name: "Percy-v2 Bot".to_string(),
            running: is_docker_container_running("percy-bot"),
            started_at: docker_container_started_at("percy-bot"),
        },
    ];
}

fn is_screen_running(name: &str) -> bool {
    match Command::new("screen").args(["-ls", name]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains(name),
        Err(_) => {
            eprintln!("Warning: `screen` not found on PATH");
            false
        }
    }
}

fn is_docker_container_running(name: &str) -> bool {
    match Command::new("docker").args(["ps"]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains(name),
        Err(_) => {
            eprintln!("Warning: `docker` not found on PATH");
            false
        }
    }
}

async fn service_action(
    account: Account,
    Form(data): Form<ServiceAction>,
) -> Result<Redirect, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    match data.action.as_str() {
        "start" => {
            if data.name == "Snekbox API" {
                let _ = Command::new("docker").args(["start", "percy-snekbox"]).status();
            } else if data.name == "Database" {
                let _ = Command::new("docker").args(["start", "percy-db"]).status();
            } else if data.name == "Percy-v2 Bot" {
                let _ = Command::new("docker").args(["start", "percy"]).status();
            } else if data.name == "Lavalink" {
                let _ = Command::new("screen").args(["-dmS", "lavalink", "/usr/bin/java", "-jar", "/home/parzival/executables/lavalink/Lavalink.jar"]).status();
            }
        }
        "stop" => {
            if data.name == "Snekbox API" {
                let _ = Command::new("docker").args(["stop", "percy-snekbox"]).status();
            } else if data.name == "Database" {
                let _ = Command::new("docker").args(["stop", "percy-db"]).status();
            } else if data.name == "Percy-v2 Bot" {
                let _ = Command::new("docker").args(["stop", "percy"]).status();
            } else if data.name == "Lavalink" {
                let _ = Command::new("screen").args(["-S", "lavalink", "-X", "quit"]).status();
            }
        }
        _ => {}
    }

    Ok(Redirect::to("/services"))
}

async fn services_index(account: Account) -> Result<ServicesTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let services = get_servicestatus();

    Ok(ServicesTemplate {
        account: Some(account),
        services,
    })
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/services", get(services_index))
        .route("/services/action", post(service_action))
}