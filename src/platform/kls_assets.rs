//! The base stylesheet and shared JS, embedded at compile time and served under
//! `/kls/*` (`base.css`, `base.js`, `timestamps.js`). Vendored from the former
//! `kls-ui` crate; sources live in `static/kls/`. Served `no-cache` so a design
//! change is never masked by a stale copy (the service worker still caches them).

use axum::{
    http::header::{CACHE_CONTROL, CONTENT_TYPE},
    response::IntoResponse,
    routing::get,
    Router,
};

pub const BASE_CSS: &str = include_str!("../../static/kls/base.css");
pub const BASE_JS: &str = include_str!("../../static/kls/base.js");
pub const TIMESTAMPS_JS: &str = include_str!("../../static/kls/timestamps.js");

fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    ([(CONTENT_TYPE, content_type), (CACHE_CONTROL, "no-cache")], body)
}

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

    #[test]
    fn assets_are_embedded() {
        assert!(BASE_CSS.contains("JetBrains"), "base.css design tokens missing");
        assert!(!BASE_JS.trim().is_empty(), "base.js empty");
        assert!(!TIMESTAMPS_JS.trim().is_empty(), "timestamps.js empty");
    }

    #[test]
    fn routes_build() {
        let _ = routes::<()>();
    }
}
