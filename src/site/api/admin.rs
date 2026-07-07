//! Admin-scoped API endpoints.
//!
//! These are the first endpoints to actually require the `admin:read` /
//! `admin:write` token scopes (previously defined but unused). They expose
//! read-only homelab state for external dashboards / automation.

use axum::{extract::State, Json};

use crate::{error::ApiError, models::Scope, updates::ImageUpdate, ApiToken, AppState};

/// List container image-update status.
///
/// > **⚠️ Internal — not for public use.** Documented for reference only. The
/// > `admin:read` / `admin:write` scopes are never granted to a normal personal
/// > API key (see [`Scope::requires_admin`]); these endpoints back the operator's
/// > own homelab dashboards.
///
/// Returns the most recent result of the background image-update checker for
/// every configured Docker service. Requires the `admin:read` scope.
#[utoipa::path(
    get,
    path = "/admin/updates",
    tag = "admin",
    security(("api_key" = ["admin:read"])),
    responses(
        (status = 200, description = "Per-service image update status", body = [ImageUpdate]),
        (status = 401, description = "Missing or invalid API key", body = ApiError),
        (status = 403, description = "API key lacks the admin:read scope", body = ApiError),
    )
)]
pub async fn list_updates(State(state): State<AppState>, token: ApiToken) -> Result<Json<Vec<ImageUpdate>>, ApiError> {
    token.require(Scope::AdminRead)?;
    let mut updates: Vec<ImageUpdate> = state.image_updates_map().into_values().collect();
    updates.sort_by(|a, b| a.service.cmp(&b.service));
    Ok(Json(updates))
}
