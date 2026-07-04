// Modules are grouped into domains so the source tree stays navigable as the
// project grows. The flat `pub use` aliases below preserve the historical
// `crate::<module>` paths, so call sites don't need to spell out the domain.
pub mod auth;
pub mod core;
pub mod integrations;
pub mod media;
pub mod services;
pub mod web;

// Flat aliases: keep every `crate::<module>` path working after the regrouping.
// `key` is re-exported from the shared kls-web-core crate so `crate::key::…`
// call sites are unchanged.
pub use auth::{token, totp};
pub use core::{cli, config, database, error, filters, logging, migrations, models, state, utils};
pub use integrations::{ai, cf_tunnel, cloudflare, discord, exttools, geoip, percy};
pub use kls_web_core::key;
pub use media::{codeimage, metadata, scan, thumbnail};
pub use services::{
    alerts, audit, backup, cron, dbadmin, docker, firewall, health, metrics, percy_moderation, percy_stats, proxy,
    secrets, ssh, updates,
};
pub use web::{cached, flash, headers, ratelimit, routes, scope};

// Curated value re-exports (the crate's public API surface).
pub use core::cli::{Command, PROGRAM_NAME};
pub use core::config::{Config, CONFIG};
pub use core::database::Database;
pub use core::state::AppState;
pub use core::utils::{MAX_BODY_SIZE, MAX_UPLOAD_SIZE};
// The `boxed_params!` macro moved into kls-web-core; re-export it at the crate
// root so existing `crate::boxed_params` call sites keep resolving.
pub use kls_web_core::boxed_params;
pub use web::routes::{copy_api_token, ApiToken};

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
