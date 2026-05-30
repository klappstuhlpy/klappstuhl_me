//! Secret-scanner dashboard routes.
//!
//! - `GET  /admin/secrets`             page (extends admin_layout)
//! - `GET  /admin/secrets/data?status` JSON: tile counts + last scan + findings list
//! - `POST /admin/secrets/scan`        trigger an immediate scan (admin only)
//! - `POST /admin/secrets/:id/status`  mark dismissed / resolved / open

use crate::{
    headers::ClientIp,
    models::Account,
    secrets::{self, FindingRow, LastScan, StatusCounts},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Form, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Template)]
#[template(path = "admin/admin_secrets.html")]
struct AdminSecretsTemplate {
    account: Option<Account>,
    active_page: &'static str,
    /// Whether `secret_scan_paths` was configured. Used by the page to
    /// show an empty-state banner directing the admin to add paths.
    scanner_enabled: bool,
}

async fn secrets_page(State(state): State<AppState>, account: Account) -> Result<AdminSecretsTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminSecretsTemplate {
        account: Some(account),
        active_page: "secrets",
        scanner_enabled: !state.config().secret_scan_paths.is_empty(),
    })
}

#[derive(Deserialize)]
struct DataQuery {
    /// "open" | "dismissed" | "resolved" | "all" (omit for "open")
    #[serde(default)]
    status: Option<String>,
}

#[derive(Serialize)]
struct SecretsData {
    counts: StatusCounts,
    last_scan: Option<LastScan>,
    findings: Vec<FindingRow>,
    scanner_enabled: bool,
}

async fn secrets_data(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<DataQuery>,
) -> Result<Json<SecretsData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let filter = match query.status.as_deref() {
        Some("all") | None => None,
        Some(s) if matches!(s, "open" | "dismissed" | "resolved") => Some(s),
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let counts = secrets::storage::status_counts(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let last_scan = secrets::storage::last_scan(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let findings = secrets::storage::list_findings(&state, filter, 200)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(SecretsData {
        counts,
        last_scan,
        findings,
        scanner_enabled: !state.config().secret_scan_paths.is_empty(),
    }))
}

#[derive(Serialize)]
struct TriggerResponse {
    started: bool,
    detail: String,
}

async fn trigger_scan(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
) -> Result<Json<TriggerResponse>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    if state.config().secret_scan_paths.is_empty() {
        return Ok(Json(TriggerResponse {
            started: false,
            detail: "No scan paths configured. Add secret_scan_paths to config.json.".into(),
        }));
    }
    state
        .audit("secret.scan.trigger")
        .actor(&account)
        .ip_opt(client_ip)
        .fire();
    // Run in the background so the HTTP call returns immediately; results
    // are visible on the next /data fetch.
    let bg = state.clone();
    tokio::spawn(async move {
        if let Err(e) = secrets::run_scan(&bg).await {
            tracing::error!(error = %e, "manual secret scan failed");
        }
    });
    Ok(Json(TriggerResponse {
        started: true,
        detail: "Scan queued — refresh in a moment.".into(),
    }))
}

#[derive(Deserialize)]
struct StatusUpdate {
    status: String,
}

async fn update_status(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
    Form(payload): Form<StatusUpdate>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    if !matches!(payload.status.as_str(), "open" | "dismissed" | "resolved") {
        return Err(StatusCode::BAD_REQUEST);
    }
    secrets::storage::set_status(&state, id, &payload.status)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("secret.status.change")
        .actor(&account)
        .target(format!("finding:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "status": payload.status }))
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/secrets", get(secrets_page))
        .route("/admin/secrets/data", get(secrets_data))
        .route("/admin/secrets/scan", post(trigger_scan))
        .route("/admin/secrets/:id/status", post(update_status))
}
