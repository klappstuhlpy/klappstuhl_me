use askama::Template;
use axum::{
    http::StatusCode,
    routing::get,
    routing::post,
    Router,
};
use axum::{Form};
use serde::Serialize;
use serde::Deserialize;
use utoipa::ToSchema;
use std::process::Command;
use axum::extract::State;
use axum::response::Redirect;
use crate::{
    models::Account,
    AppState,
};

#[derive(Debug, Serialize, PartialEq, Eq, Clone, ToSchema)]
pub struct ServiceEntry {
    /// The service's name.
    pub(crate) name: String,
    /// Whether the service is running.
    pub(crate) running: bool,
    /// The uptime of the service, if available.
    pub(crate) uptime: String,
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

fn docker_container_uptime(name: &str) -> Option<String> {
    match Command::new("docker")
        .args(["ps", "--filter", &format!("name={}", name), "--format", "{{.RunningFor}}"])
        .output()
    {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() { Some(s) } else { None }
        }
        Err(_) => None,
    }
}

fn screen_uptime(name: &str) -> Option<String> {
    match Command::new("pgrep").args(["-af", name]).output() {
        Ok(out) => {
            if out.stdout.is_empty() {
                return None;
            }
            // crude: get elapsed time of first PID
            let pid = String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();

            if pid.is_empty() {
                return None;
            }

            let etime_out = Command::new("ps").args(["-o", "etime=", "-p", &pid]).output().ok()?;
            let etime = String::from_utf8_lossy(&etime_out.stdout).trim().to_string();
            if etime.is_empty() { None } else { Some(etime) }
        }
        Err(_) => None,
    }
}

fn get_servicestatus() -> Vec<ServiceEntry> {
    return vec![
        ServiceEntry {
            name: "Lavalink".to_string(),
            running: is_screen_running("lavalink"),
            uptime: screen_uptime("lavalink").unwrap_or("N/A".to_string()).to_string(),
        },
        ServiceEntry {
            name: "Snekbox API".to_string(),
            running: is_docker_container_running("snekbox"),
            uptime: docker_container_uptime("snekbox").unwrap_or("N/A".to_string()).to_string(),
        },
        ServiceEntry {
            name: "Database".to_string(),
            running: is_docker_container_running("postgres"),
            uptime: docker_container_uptime("postgres").unwrap_or("N/A".to_string()).to_string(),
        },
        ServiceEntry {
            name: "Percy-v2 Bot".to_string(),
            running: is_docker_container_running("percy-bot"),
            uptime: docker_container_uptime("percy-bot").unwrap_or("N/A".to_string()).to_string(),
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
    State(state): State<AppState>,
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