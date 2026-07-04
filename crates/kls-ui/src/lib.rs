//! Shared klappstuhl.me design system.
//!
//! Holds the design foundation used by **both** the klappstuhl.me main site /
//! admin shell and the Percy dashboard: the base stylesheet (design tokens) and
//! the two JS utilities every page pulls in (theme bootstrap + relative-time
//! humanisation). Assets are embedded at compile time via [`include_str!`], so
//! the crate carries no runtime file-path dependency and serves identical bytes
//! from whichever binary links it.
//!
//! Mount [`routes`] under a stable prefix (klappstuhl_me nests it at `/kls`,
//! e.g. `/kls/base.css`). This is the single source of truth for the shared
//! look-and-feel — see DASHBOARD_DECOUPLING_PLAN.md (Phase 2). The crate
//! graduates to the standalone `klappstuhl-shared` repo at Phase 5.

use axum::{
    http::header::{CACHE_CONTROL, CONTENT_TYPE},
    response::IntoResponse,
    routing::get,
    Router,
};

/// Design tokens / base stylesheet shared across every page. `@import`s the
/// JetBrains Mono webfont; otherwise self-contained.
pub const BASE_CSS: &str = include_str!("../assets/base.css");

/// Theme bootstrap + shared page glue (drives the light/dark toggle).
pub const BASE_JS: &str = include_str!("../assets/base.js");

/// Humanises `.js-ts` elements into relative timestamps with hover tooltips.
pub const TIMESTAMPS_JS: &str = include_str!("../assets/timestamps.js");

/// Builds a response for an embedded text asset with its content type. Assets
/// are served `no-cache` so a design change is never masked by a stale copy;
/// the service worker still caches them for offline/repeat loads and busts on
/// its own version bump.
fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    ([(CONTENT_TYPE, content_type), (CACHE_CONTROL, "no-cache")], body)
}

/// Router serving the shared design assets at bare paths (`/base.css`,
/// `/base.js`, `/timestamps.js`). Generic over router state so it merges/nests
/// into any app's `Router<S>`; the handlers ignore state entirely.
pub fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/base.css",
            get(|| async { asset("text/css; charset=utf-8", BASE_CSS) }),
        )
        .route(
            "/base.js",
            get(|| async { asset("text/javascript; charset=utf-8", BASE_JS) }),
        )
        .route(
            "/timestamps.js",
            get(|| async { asset("text/javascript; charset=utf-8", TIMESTAMPS_JS) }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the `include_str!` wiring: a broken asset path or emptied file
    /// would make these constants empty/wrong at compile or run time.
    #[test]
    fn assets_are_embedded() {
        // base.css opens with the JetBrains Mono webfont @import.
        assert!(BASE_CSS.contains("JetBrains"), "base.css design tokens missing");
        assert!(!BASE_JS.trim().is_empty(), "base.js empty");
        assert!(!TIMESTAMPS_JS.trim().is_empty(), "timestamps.js empty");
    }

    /// The router must build without panicking (route registration).
    #[test]
    fn routes_build() {
        let _ = routes::<()>();
    }
}
