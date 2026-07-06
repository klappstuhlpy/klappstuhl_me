use crate::AppState;
use crate::{flash::Flashes, models::Account};
use askama::Template;
use axum::{
    extract::State,
    response::IntoResponse,
    response::{Redirect, Response},
    routing::get,
    Router,
};

mod admin;
mod api;
mod ask;
mod audit;
mod auth;
mod backups;
mod certs;
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

async fn index(State(state): State<AppState>, account: Option<Account>, flashes: Flashes) -> Response {
    IndexTemplate {
        account,
        flashes,
        url: state.config().canonical_url(),
    }
    .into_response()
}

/// The standalone projects page was folded into the homepage (`/#projects`).
/// Kept as a redirect so old bookmarks, the spotlight palette, and the AI
/// `navigate` tool's whitelisted `/projects` route still resolve.
async fn projects() -> impl IntoResponse {
    Redirect::to("/#projects")
}

/// Redirect to the standalone Percy dashboard. Respects dev/prod environment so
/// the link in layout.html doesn't need to be hardcoded to a specific domain.
///
/// When SSO is configured (`sso_secret` set on both apps) and the visitor is
/// logged in with a linked Discord account, this vouches their identity to the
/// dashboard with a short-lived signed handoff so they arrive already signed in.
/// Otherwise it's a plain redirect and the dashboard does its own Discord login.
async fn percy_redirect(State(state): State<AppState>, account: Option<Account>) -> Response {
    let config = state.config();
    let percy_url = config.percy_url();

    if let (Some(account), Some(secret)) = (account.as_ref(), config.sso_secret.as_ref()) {
        if let Some(discord_id) = account.discord_id.as_ref() {
            let handoff = kls_web_core::sso::Handoff::new(
                discord_id.clone(),
                account.name.clone(),
                kls_web_core::sso::DEFAULT_TTL_SECS,
            );
            if let Some(token) = handoff.sign(secret) {
                return Redirect::to(&format!("{percy_url}/auth/handoff?t={token}")).into_response();
            }
        }
    }

    Redirect::to(&percy_url).into_response()
}

pub fn all() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/projects", get(projects))
        .route("/percy", get(percy_redirect))
        .route("/m/:id", get(api::serve_media))
        .merge(auth::routes())
        .merge(discord_oauth::routes())
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
        .merge(ask::routes())
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
