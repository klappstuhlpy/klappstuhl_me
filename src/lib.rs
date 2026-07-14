// The source tree is organised feature-first around the two product surfaces:
// `site` (the public klappstuhl.me website + JSON API) and `admin` (the
// self-hosting control panel), each holding vertical feature slices that pair
// HTTP routes with their backing service. `core` (fundamentals), `platform`
// (shared HTTP plumbing), `auth` (crypto primitives) and `integrations`
// (third-party clients) are the cross-cutting layers both surfaces build on.
// `routes` stitches the two surfaces into one router.
//
// The flat `pub use` aliases below preserve the historical `crate::<module>`
// paths, so call sites don't need to spell out the surface a module lives in.
pub mod admin;
pub mod auth;
pub mod core;
pub mod integrations;
pub mod platform;
pub mod routes;
pub mod site;

// Flat aliases: keep every `crate::<module>` path working after the regrouping.
// `key` is re-exported from the shared kls-web-core crate so `crate::key::…`
// call sites are unchanged.
pub use auth::{token, totp};
pub use core::{cli, config, database, error, filters, logging, migrations, models, state, utils};
pub use integrations::{ai, cf_tunnel, cloudflare, discord, exttools, geoip};
pub use kls_web_core::key;
pub use platform::{cached, cookies, flash, headers, ratelimit, scope};
pub use site::media::{codeimage, metadata, scan, thumbnail};
// Each admin feature slice keeps its service's public API at the module root,
// so `crate::firewall::spawn_workers`, `crate::docker::…`, etc. are unchanged.
pub use admin::{
    alerts, audit, backup, cron, dbadmin, docker, firewall, health, metrics, proxy, secrets, ssh, updates,
};

/// The running version, taken from `Cargo.toml` — the single source of truth for
/// it. The site footer, the changelog page and the OpenAPI docs all derive from
/// this; never hardcode a version string anywhere else (see
/// `.claude/CHANGELOG_GUIDE.md`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// Curated value re-exports (the crate's public API surface).
pub use core::cli::{Command, PROGRAM_NAME};
pub use core::config::{Config, CONFIG};
pub use core::database::Database;
pub use core::state::AppState;
pub use core::utils::{MAX_BODY_SIZE, MAX_UPLOAD_SIZE};
// The `boxed_params!` macro moved into kls-web-core; re-export it at the crate
// root so existing `crate::boxed_params` call sites keep resolving.
pub use kls_web_core::boxed_params;
pub use routes::{copy_api_token, ApiToken};

/// A middleware responsible for parsing cookies into a Vec<Cookie> extension for use
/// for other cookie-related middleware.
///
/// This middleware must come *after* the cookie related middlewares.
pub async fn parse_cookies(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let cookies = req
        .headers()
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| cookie::Cookie::parse_encoded(cookie.trim().to_owned()).ok())
        .collect::<Vec<_>>();

    req.extensions_mut().insert(cookies);
    next.run(req).await
}
