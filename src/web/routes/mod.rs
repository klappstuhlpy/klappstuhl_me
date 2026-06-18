use crate::AppState;
use crate::{flash::Flashes, models::Account};
use askama::Template;
use axum::{extract::State, response::IntoResponse, response::Redirect, routing::get, Router};

mod admin;
mod api;
mod audit;
mod auth;
mod backups;
mod certs;
mod dashboard;
mod dbadmin;
mod discord_oauth;
mod docker;
mod firewall;
mod health;
mod image;
mod links;
mod logs;
mod metrics;
mod proxy;
mod sanitizer;
mod secrets;
mod security;
mod spotlight;
mod ssh;
mod terminal;
mod ws;

pub use api::{copy_api_token, ApiToken};
pub use image::spawn_expiry_reaper;

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    account: Option<Account>,
    flashes: Flashes,
    url: String,
}

async fn index(State(state): State<AppState>, account: Option<Account>, flashes: Flashes) -> impl IntoResponse {
    IndexTemplate {
        account,
        flashes,
        url: state.config().canonical_url(),
    }
}

/// The standalone projects page was folded into the homepage (`/#projects`).
/// Kept as a redirect so old bookmarks, the spotlight palette, and the AI
/// `navigate` tool's whitelisted `/projects` route still resolve.
async fn projects() -> impl IntoResponse {
    Redirect::to("/#projects")
}

pub fn all() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/projects", get(projects))
        .route("/m/:id", get(api::serve_media))
        .merge(auth::routes())
        .merge(discord_oauth::routes())
        .merge(dashboard::routes())
        .merge(image::routes())
        .merge(links::routes())
        .merge(admin::routes())
        .merge(audit::routes())
        .merge(metrics::routes())
        .merge(dbadmin::routes())
        .merge(secrets::routes())
        .merge(security::routes())
        .merge(ssh::routes())
        .merge(docker::routes())
        .merge(firewall::routes())
        .merge(health::routes())
        .merge(proxy::routes())
        .merge(sanitizer::routes())
        .merge(certs::routes())
        .merge(backups::routes())
        .merge(logs::routes())
        .merge(spotlight::routes())
        .merge(terminal::routes())
        .merge(ws::routes())
        .nest("/api", api::routes())
        // Resolves bare `r.<domain>/<code>` short links; 404s everything else.
        .fallback(links::short_link_fallback)
}

#[cfg(test)]
mod tests {
    /// Building the whole router exercises matchit's route registration,
    /// which panics on conflicting paths (e.g. a static segment overlapping a
    /// `:param` on axum 0.7). This catches such conflicts as a test failure
    /// rather than at server start-up.
    #[test]
    fn full_router_builds() {
        let _ = super::all();
    }
}
