use crate::AppState;
use crate::{flash::Flashes, models::Account};
use askama::Template;
use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::IntoResponse,
    response::{Redirect, Response},
    routing::get,
    Router,
};

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

/// Returns `true` if the request's Host header indicates the percy subdomain.
fn is_percy_host(headers: &HeaderMap) -> bool {
    headers.get("host").and_then(|h| h.to_str().ok()).is_some_and(|h| {
        let host = h.split(':').next().unwrap_or(h);
        host.starts_with("percy.")
    })
}

/// Paths that belong on the percy subdomain. Requests to these paths on the
/// apex get redirected to percy, and requests to other paths on the percy
/// subdomain get redirected to the apex.
fn is_percy_path(path: &str) -> bool {
    path == "/"
        || path.starts_with("/dashboard")
        || path.starts_with("/lb")
        || path == "/commands"
        || path == "/changelog"
        || path == "/privacy-policy"
        || path == "/terms-of-service"
        || path == "/main-site"
}

/// Paths that should work on ANY host (auth, static, api, ws, media).
fn is_shared_path(path: &str) -> bool {
    path.starts_with("/static")
        || path.starts_with("/api")
        || path.starts_with("/ws")
        || path.starts_with("/login")
        || path.starts_with("/signup")
        || path.starts_with("/logout")
        || path.starts_with("/auth")
        || path.starts_with("/m/")
        || path == "/sw.js"
        || path == "/account/discord/avatar"
}

/// Middleware that enforces subdomain boundaries. Percy paths on the apex
/// redirect to percy; non-percy paths on the percy subdomain redirect to apex.
pub async fn enforce_subdomain(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let headers = req.headers();
    let percy = is_percy_host(headers);

    if is_shared_path(path) {
        return next.run(req).await;
    }

    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();

    if percy && !is_percy_path(path) {
        let canonical = state.config().canonical_url();
        return Redirect::to(&format!("{canonical}{path}{query}")).into_response();
    }

    if !percy && is_percy_path(path) && path != "/" {
        let percy_url = state.config().percy_url();
        return Redirect::to(&format!("{percy_url}{path}{query}")).into_response();
    }

    next.run(req).await
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    account: Option<Account>,
    flashes: Flashes,
    url: String,
}

#[derive(Template)]
#[template(path = "percy/home.html")]
struct PercyHomeTemplate {
    account: Option<Account>,
    #[allow(dead_code)]
    flashes: Flashes,
}

async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
    account: Option<Account>,
    flashes: Flashes,
) -> Response {
    if is_percy_host(&headers) {
        return PercyHomeTemplate { account, flashes }.into_response();
    }
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

/// Redirect to the Percy subdomain. Respects dev/prod environment so the link
/// in layout.html doesn't need to be hardcoded to a specific domain.
async fn percy_redirect(State(state): State<AppState>) -> impl IntoResponse {
    Redirect::to(&state.config().percy_url())
}

/// Redirect to the main site (apex domain). Used by the percy subdomain footer
/// to link back without hardcoding the domain.
async fn main_site_redirect(State(state): State<AppState>) -> impl IntoResponse {
    Redirect::to(&state.config().canonical_url())
}

pub fn all() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/projects", get(projects))
        .route("/percy", get(percy_redirect))
        .route("/main-site", get(main_site_redirect))
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
