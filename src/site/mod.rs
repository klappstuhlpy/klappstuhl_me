//! The public website surface at klappstuhl.me: the homepage, the image hoster,
//! short links, the code screenshot / spotlight tools, the AI `ask` endpoint,
//! account/login pages, and the documented public JSON API (`site::api`). Each
//! feature owns its handlers here; [`routes`] assembles them into the public
//! sub-router that `crate::routes::all` merges with the admin shell.

use crate::AppState;
use crate::{flash::Flashes, models::Account};
use askama::Template;
use axum::{
    extract::State,
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};

pub mod account;
pub mod api;
pub mod ask;
pub mod discord_oauth;
pub mod image;
pub mod links;
pub mod media;
pub mod paste;
pub mod spotlight;

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

/// The public website sub-router. The short-link fallback is attached at the top
/// level in [`crate::routes::all`] so it applies after admin routes too.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/projects", get(projects))
        .route("/percy", get(percy_redirect))
        .route("/m/:id", get(api::serve_media))
        .merge(account::routes())
        .merge(discord_oauth::routes())
        .merge(image::routes())
        .merge(links::routes())
        .merge(paste::routes())
        .merge(spotlight::routes())
        .merge(ask::routes())
        .nest("/api", api::routes())
}
