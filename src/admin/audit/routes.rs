//! `/admin/audit` dashboard — reads what the `crate::audit` helper writes.

use crate::{
    audit::{counts, list_recent, AuditCounts, AuditEntry},
    models::Account,
    AppState,
};
use askama::Template;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};

#[derive(Template)]
#[template(path = "admin/admin_audit.html")]
struct AdminAuditTemplate {
    account: Option<Account>,
    active_page: &'static str,
}

async fn audit_page(account: Account) -> Result<AdminAuditTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminAuditTemplate {
        account: Some(account),
        active_page: "audit",
    })
}

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    100
}

#[derive(Serialize)]
struct AuditData {
    counts: AuditCounts,
    entries: Vec<AuditEntry>,
}

async fn audit_data(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let counts = counts(&state).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entries = list_recent(
        &state,
        query.action.as_deref().filter(|s| !s.is_empty()),
        query.actor.as_deref().filter(|s| !s.is_empty()),
        query.limit.clamp(1, 500),
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AuditData { counts, entries }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/audit", get(audit_page))
        .route("/admin/audit/data", get(audit_data))
}
