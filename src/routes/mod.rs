use crate::{
    cached::BodyCache,
    flash::Flashes,
    headers::{AcceptEncoding},
    models::{Account},
    filters
};
use askama::Template;
use axum::{
    extract::{State},
    response::IntoResponse,
    routing::get,
    Extension, Router,
};
use crate::{AppState};

mod admin;
mod audit;
mod auth;
mod image;
mod api;

pub use api::{copy_api_token, ApiToken};
use crate::models::{ProjectEntry, ProjectList};

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
    encoding: AcceptEncoding,
    Extension(cacher): Extension<BodyCache>,
) -> impl IntoResponse {
    let template = IndexTemplate {
        account,
        flashes,
        url: state.config().canonical_url()
    };
    cacher.cache_template("index", template, encoding, false).await
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
    encoding: AcceptEncoding,
    Extension(cacher): Extension<BodyCache>,
) -> impl IntoResponse {
    let template = ProjectsTemplate {
        account,
        flashes,
        url: state.config().url_to("/projects"),
    };
    cacher.cache_template("projects", template, encoding, false).await
}

#[derive(Template)]
#[template(path = "help.html")]
struct HelpTemplate {
    account: Option<Account>,
}

async fn help_page(account: Option<Account>) -> impl IntoResponse {
    HelpTemplate { account }
}

#[derive(Template)]
#[template(path = "contact.html")]
struct ContactTemplate {
    account: Option<Account>,
}

async fn contact_page(account: Option<Account>) -> impl IntoResponse {
    ContactTemplate { account }
}

pub fn all() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/projects", get(projects))
        .route("/help", get(help_page))
        .route("/contact", get(contact_page))
        .merge(auth::routes())
        .merge(image::routes())
        .merge(admin::routes())
        .merge(audit::routes())
        .nest("/api", api::routes())
}