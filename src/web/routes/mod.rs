use crate::AppState;
use crate::{flash::Flashes, models::Account};
use askama::Template;
use axum::{
    extract::{Request, State},
    http::{header::HOST, Uri},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
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

/// The closed set of public URL prefixes the Percy dashboard is reachable at on
/// its subdomain (the `/percy` route-table prefix is stripped from the public
/// URL). Anything else on the subdomain — `/static`, `/ws`, `/login`,
/// `/account`, … — is a request for the *shared* site routes and passes through
/// untouched. `/` is included so the bare subdomain lands on the dashboard.
fn is_percy_dashboard_path(path: &str) -> bool {
    const ENTRIES: &[&str] = &["/dashboard", "/lb", "/privacy-policy", "/terms-of-service"];
    path == "/" || ENTRIES.iter().any(|p| path == *p || path.starts_with(&format!("{p}/")))
}

/// Builds a path-only [`Uri`] from a path-and-query string, preserving any query.
fn path_uri(path_and_query: &str) -> Option<Uri> {
    path_and_query.parse().ok()
}

/// Host-based routing for the Percy dashboard subdomain.
///
/// The dashboard's routes are registered under a `/percy/...` prefix (see
/// [`dashboard::routes`]), but its public URL is the bare `percy.<domain>`
/// subdomain. This middleware bridges the two:
///
/// * On the dashboard host (`config.percy_domain()`): a public dashboard path
///   (`/dashboard`, `/lb`, …) is internally rewritten to `/percy/...` so the
///   existing route table matches, while shared paths pass through.
/// * On the production apex: legacy `/percy/*` links **and** bare dashboard
///   entry paths are 301-redirected to the subdomain, so old bookmarks and the
///   main-site "Dashboard" nav link land on `percy.<domain>`.
///
/// In dev the apex redirect is skipped (there's no public subdomain to send a
/// browser to beyond `percy.localhost`, and devs may use the path-based
/// `/percy/...` fallback on `localhost`).
pub async fn percy_host_rewrite(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let config = state.config();

    let host = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| req.headers().get("x-forwarded-host").and_then(|h| h.to_str().ok()))
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    let on_dashboard_host = host == config.percy_domain().to_ascii_lowercase();

    if on_dashboard_host {
        let path = req.uri().path();
        if is_percy_dashboard_path(path) && !path.starts_with("/percy") {
            let pq = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or(path);
            // Strip the leading slash for the bare root so we get `/percy`
            // (+ query) rather than the unmatched `/percy/`.
            let rewritten = if path == "/" {
                format!("/percy{}", &pq[1..])
            } else {
                format!("/percy{pq}")
            };
            if let Some(uri) = path_uri(&rewritten) {
                *req.uri_mut() = uri;
            }
        }
        return next.run(req).await;
    }

    if config.production {
        let path = req.uri().path();
        let pq = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or(path);
        // Legacy `/percy/*` (and the bare `/percy`) → subdomain without the prefix.
        // This covers old bookmarks to every dashboard page and the public
        // leaderboard / legal docs.
        if path == "/percy" || path.starts_with("/percy/") {
            let rest = &pq["/percy".len()..];
            let target = if rest.starts_with('/') { rest } else { "/dashboard" };
            return Redirect::permanent(&config.percy_url(target)).into_response();
        }
        // The main-site nav's "Dashboard" link is the bare `/dashboard`; send it
        // to the subdomain. Scoped to `/dashboard` only (not `/lb`, `/privacy-policy`,
        // …) so the apex stays free to define those paths itself — the public
        // dashboard pages are only ever linked as absolute `percy.<domain>` URLs.
        if path == "/dashboard" || path.starts_with("/dashboard/") {
            return Redirect::permanent(&config.percy_url(pq)).into_response();
        }
    }

    next.run(req).await
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
