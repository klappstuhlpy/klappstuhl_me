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
pub use auth::{key, token, totp};
pub use core::{cli, config, database, error, filters, logging, models, state, utils};
pub use integrations::{cloudflare, discord, exttools, geoip};
pub use media::{codeimage, metadata, scan};
pub use services::{
    alerts, audit, backup, cron, docker, firewall, health, metrics, postgres, proxy, secrets, ssh, updates,
};
pub use web::{cached, flash, headers, ratelimit, routes, scope};

// Curated value re-exports (the crate's public API surface).
pub use core::cli::{Command, PROGRAM_NAME};
pub use core::config::{Config, CONFIG};
pub use core::database::Database;
pub use core::state::AppState;
pub use core::utils::{MAX_BODY_SIZE, MAX_UPLOAD_SIZE};
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
