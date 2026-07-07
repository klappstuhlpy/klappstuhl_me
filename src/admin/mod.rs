//! The admin shell: the self-hosting control panel for the box Klappstuhl runs
//! on. Every feature here is a vertical slice — its HTTP handlers (`routes.rs`
//! for folder features, the module root for single-file ones) live next to the
//! long-running backend that powers them (metrics collection, Docker/firewall/
//! health/proxy control, secret scanning, SSH, backups, audit log, …).
//!
//! [`routes`] assembles the admin sub-router; `main.rs` starts each feature's
//! background workers via the `crate::<feature>::spawn_*` aliases (see `lib.rs`).

use crate::AppState;
use axum::Router;

// Feature slices that pair a background service with HTTP routes.
pub mod audit;
pub mod backup;
pub mod dbadmin;
pub mod docker;
pub mod firewall;
pub mod health;
pub mod metrics;
pub mod proxy;
pub mod secrets;
pub mod ssh;

// Service-only slices (background workers reached through crate aliases; their
// data is surfaced via other features' routes or the public API).
pub mod alerts;
pub mod cron;
pub mod updates;

// Route-only slices (thin admin pages with no dedicated background service).
pub mod certs;
pub mod dashboard;
pub mod logs;
pub mod sanitizer;
pub mod security;
pub mod ws;

/// The admin shell sub-router. Merged into the full application router by
/// [`crate::routes::all`].
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(dashboard::routes())
        .merge(audit::routes::routes())
        .merge(metrics::routes::routes())
        .merge(dbadmin::routes::routes())
        .merge(secrets::routes::routes())
        .merge(security::routes())
        .merge(ssh::routes::routes())
        .merge(docker::routes::routes())
        .merge(firewall::routes::routes())
        .merge(health::routes::routes())
        .merge(proxy::routes::routes())
        .merge(sanitizer::routes())
        .merge(certs::routes())
        .merge(backup::routes::routes())
        .merge(logs::routes())
        .merge(ws::routes())
}
