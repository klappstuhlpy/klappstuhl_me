//! Top-level router assembly. Stitches the public website (`site`) into the
//! axum `Router`, and re-exports the HTTP entry points `main.rs` drives (`all`,
//! `spawn_expiry_reaper`, the API-token middleware).

use crate::AppState;
use axum::Router;

pub use crate::site::api::{copy_api_token, ApiToken};
pub use crate::site::image::spawn_expiry_reaper;
pub use crate::site::paste::spawn_paste_reaper;

/// Builds the complete application router.
pub fn all() -> Router<AppState> {
    crate::site::routes()
        // Resolves bare `r.<domain>/<code>` short links; 404s everything else.
        .fallback(crate::site::links::short_link_fallback)
}

#[cfg(test)]
mod tests {
    /// Building the whole router exercises matchit's route registration, which
    /// panics on conflicting paths (e.g. a static segment overlapping a `:param`
    /// on axum 0.7). This catches such conflicts as a test failure rather than
    /// at server start-up.
    #[test]
    fn full_router_builds() {
        let _ = super::all();
    }
}
