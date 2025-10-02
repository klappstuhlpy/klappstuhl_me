use askama::Template;
use axum::{
    http::StatusCode,
    routing::get,
    Router,
};
use serde::Serialize;
use utoipa::ToSchema;
use std::process::Command;
use crate::{
    models::Account,
    AppState,
};
use crate::flash::Flashes;

#[derive(Debug, Serialize, PartialEq, Eq, Clone, ToSchema)]
pub struct ServiceEntry {
    /// The service's name.
    pub(crate) name: String,
    /// Whether the service is running.
    pub(crate) running: bool,
}

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesTemplate {
    account: Option<Account>,
    flashes: Flashes,
    services: Vec<ServiceEntry>,
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

async fn services_index(
    account: Account,
    flashes: Flashes,
) -> Result<ServicesTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let services = vec![
        ServiceEntry {
            name: "Lavalink".to_string(),
            running: is_screen_running("lavalink"),
        },
        ServiceEntry {
            name: "Snekbox API".to_string(),
            running: is_docker_container_running("snekbox"),
        },
        ServiceEntry {
            name: "Database".to_string(),
            running: is_docker_container_running("postgres"),
        },
        ServiceEntry {
            name: "Percy-v2 Bot".to_string(),
            running: is_docker_container_running("percy-bot"),
        },
    ];

    Ok(ServicesTemplate {
        account: Some(account),
        flashes,
        services,
    })
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/services", get(services_index))
}