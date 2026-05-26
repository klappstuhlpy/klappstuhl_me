use crate::{
    flash::Flashes,
    models::{Account}
};
use askama::Template;
use axum::{
    extract::{State},
    response::IntoResponse,
    routing::get,
    Router,
};
use crate::{AppState};

mod admin;
mod audit;
mod auth;
mod docker;
mod image;
mod api;
mod metrics;
mod postgres;
mod secrets;
mod security;
mod services;
mod ssh;
mod ws;

pub use api::{copy_api_token, ApiToken};

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    account: Option<Account>,
    flashes: Flashes,
    url: String,
}

async fn index(
    State(state): State<AppState>,
    account: Option<Account>,
    flashes: Flashes,
) -> impl IntoResponse {
    IndexTemplate {
        account,
        flashes,
        url: state.config().canonical_url()
    }
}

#[derive(Template)]
#[template(path = "projects.html")]
struct ProjectsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    url: String,
}

async fn projects(
    State(state): State<AppState>,
    account: Option<Account>,
    flashes: Flashes,
) -> impl IntoResponse {
    ProjectsTemplate {
        account,
        flashes,
        url: state.config().url_to("/projects"),
    }
}

pub fn all() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/projects", get(projects))
        .merge(auth::routes())
        .merge(image::routes())
        .merge(admin::routes())
        .merge(audit::routes())
        .merge(metrics::routes())
        .merge(postgres::routes())
        .merge(secrets::routes())
        .merge(security::routes())
        .merge(services::routes())
        .merge(ssh::routes())
        .merge(docker::routes())
        .merge(ws::routes())
        .nest("/api", api::routes())
}