//! Health checks / uptime monitoring dashboard.
//!
//! - `GET    /admin/health`                  page
//! - `GET    /admin/health/data`             JSON: target list with summary
//! - `GET    /admin/health/incidents`        JSON: recent incidents (timeline)
//! - `GET    /admin/health/:id/history`      JSON: samples + stats for one target
//! - `POST   /admin/health`                  create a new target
//! - `POST   /admin/health/:id`              update target
//! - `POST   /admin/health/:id/toggle`       enable/disable a target
//! - `POST   /admin/health/:id/check`        run a probe immediately
//! - `DELETE /admin/health/:id`              delete a target

use crate::{
    headers::ClientIp,
    health::{self, checker::CheckKind, storage::NewTarget},
    models::Account,
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Form, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Template)]
#[template(path = "admin_health.html")]
struct AdminHealthTemplate {
    account: Option<Account>,
    active_page: &'static str,
}

// ─── Public status page ─────────────────────────────────────────────────────

/// A monitor as shown on the public status page. Deliberately omits the raw
/// target address, kind config, and any internal detail — only the display
/// name and a coarse status/uptime are exposed.
struct PublicService {
    name: String,
    /// Machine status used as a CSS class: `up`, `down`, `degraded`, `unknown`.
    status: String,
    status_label: &'static str,
    uptime: String,
    last_check: String,
}

#[derive(Template)]
#[template(path = "status.html")]
struct StatusTemplate {
    account: Option<Account>,
    services: Vec<PublicService>,
    overall: &'static str,
    overall_label: &'static str,
    up: usize,
    total: usize,
}

fn status_label(status: &str) -> &'static str {
    match status {
        "up" => "Operational",
        "degraded" => "Degraded",
        "down" => "Down",
        _ => "Unknown",
    }
}

/// Public, unauthenticated uptime status page built from the health monitors.
async fn status_page(
    State(state): State<AppState>,
    account: Option<Account>,
) -> Result<StatusTemplate, StatusCode> {
    let summaries = health::storage::list_summaries(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut services = Vec::new();
    let (mut up, mut down, mut degraded) = (0usize, 0usize, 0usize);
    for s in summaries.into_iter().filter(|s| s.target.enabled) {
        let status = s.last_status.clone().unwrap_or_else(|| "unknown".to_string());
        match status.as_str() {
            "up" => up += 1,
            "down" => down += 1,
            "degraded" => degraded += 1,
            _ => {}
        }
        let last_check = s
            .last_check
            .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
            .unwrap_or_else(|| "—".to_string());
        services.push(PublicService {
            name: s.target.name,
            status_label: status_label(&status),
            status,
            uptime: format!("{:.2}%", s.uptime_24h),
            last_check,
        });
    }

    let total = services.len();
    let (overall, overall_label) = if total == 0 {
        ("unknown", "No monitors configured")
    } else if down > 0 {
        ("down", "Major outage")
    } else if degraded > 0 {
        ("degraded", "Degraded performance")
    } else if up == total {
        ("up", "All systems operational")
    } else {
        ("unknown", "Status unknown")
    };

    Ok(StatusTemplate { account, services, overall, overall_label, up, total })
}

async fn page(account: Account) -> Result<AdminHealthTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminHealthTemplate {
        account: Some(account),
        active_page: "health",
    })
}

#[derive(Serialize)]
struct DashboardData {
    summaries: Vec<health::TargetSummary>,
    open_incidents: Vec<health::IncidentRow>,
    total_targets: i64,
    up_count: i64,
    down_count: i64,
    degraded_count: i64,
}

async fn data(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<DashboardData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let summaries = health::storage::list_summaries(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let open_incidents = health::storage::list_incidents(&state, None, 50)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .filter(|i| i.ended_at.is_none())
        .collect::<Vec<_>>();

    let total_targets = summaries.len() as i64;
    let mut up = 0i64;
    let mut down = 0i64;
    let mut degraded = 0i64;
    for s in &summaries {
        match s.last_status.as_deref() {
            Some("up") => up += 1,
            Some("degraded") => degraded += 1,
            Some("down") => down += 1,
            _ => {}
        }
    }

    Ok(Json(DashboardData {
        summaries,
        open_incidents,
        total_targets,
        up_count: up,
        down_count: down,
        degraded_count: degraded,
    }))
}

#[derive(Deserialize)]
struct IncidentsQuery {
    #[serde(default)]
    target_id: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn incidents(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<IncidentsQuery>,
) -> Result<Json<Vec<health::IncidentRow>>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows = health::storage::list_incidents(&state, query.target_id, limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rows))
}

#[derive(Serialize)]
struct HistoryResponse {
    target: health::HealthTarget,
    stats: health::UptimeStats,
    samples: Vec<health::SampleRow>,
    incidents: Vec<health::IncidentRow>,
}

async fn history(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
) -> Result<Json<HistoryResponse>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let target = health::storage::get_target(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let stats = health::storage::uptime_stats(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let samples = health::storage::list_samples(&state, id, 500)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let incidents = health::storage::list_incidents(&state, Some(id), 50)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(HistoryResponse {
        target,
        stats,
        samples,
        incidents,
    }))
}

#[derive(Deserialize)]
struct UpsertForm {
    name: String,
    kind: String,
    target: String,
    #[serde(default)]
    interval_seconds: Option<i64>,
    #[serde(default)]
    timeout_ms: Option<i64>,
    #[serde(default)]
    degraded_ms: Option<i64>,
    #[serde(default)]
    enabled: Option<String>,
    /// Free-form per-kind config — keyword, expected_status, warn_days, etc.
    #[serde(default)]
    config_json: Option<String>,
}

impl UpsertForm {
    fn validate(self) -> Result<NewTarget, StatusCode> {
        let kind = self.kind.trim().to_string();
        if CheckKind::from_str(&kind).is_none() {
            return Err(StatusCode::BAD_REQUEST);
        }
        let name = self.name.trim().to_string();
        let target = self.target.trim().to_string();
        if name.is_empty() || target.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        let config_json = self
            .config_json
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "{}".to_string());
        // Validate JSON shape so we don't store garbage that breaks the checker.
        if serde_json::from_str::<serde_json::Value>(&config_json).is_err() {
            return Err(StatusCode::BAD_REQUEST);
        }
        let enabled = matches!(self.enabled.as_deref(), Some("on" | "true" | "1"));
        Ok(NewTarget {
            name,
            kind,
            target,
            config_json,
            interval_seconds: self.interval_seconds.unwrap_or(60).clamp(10, 86_400),
            timeout_ms: self.timeout_ms.unwrap_or(5_000).clamp(500, 60_000),
            degraded_ms: self.degraded_ms.unwrap_or(1_000).clamp(50, 60_000),
            enabled,
        })
    }
}

async fn create(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Form(form): Form<UpsertForm>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let new = form.validate()?;
    let name = new.name.clone();
    let id = health::storage::create_target(&state, new)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("health.target.create")
        .actor(&account)
        .target(format!("health:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "name": name }))
        .fire();
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn update(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
    Form(form): Form<UpsertForm>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let new = form.validate()?;
    let name = new.name.clone();
    health::storage::update_target(&state, id, new)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("health.target.update")
        .actor(&account)
        .target(format!("health:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "name": name }))
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

async fn remove(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    health::storage::delete_target(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("health.target.delete")
        .actor(&account)
        .target(format!("health:{id}"))
        .ip_opt(client_ip)
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ToggleForm {
    enabled: String,
}

async fn toggle(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
    Form(form): Form<ToggleForm>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let enabled = matches!(form.enabled.as_str(), "on" | "true" | "1");
    health::storage::set_enabled(&state, id, enabled)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("health.target.toggle")
        .actor(&account)
        .target(format!("health:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "enabled": enabled }))
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

async fn check_now(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<Json<health::CheckOutcome>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let outcome = health::run_check_now(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("health.target.probe")
        .actor(&account)
        .target(format!("health:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "status": outcome.status_str() }))
        .fire();
    Ok(Json(outcome))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/status", get(status_page))
        .route("/admin/health", get(page))
        .route("/admin/health/data", get(data))
        .route("/admin/health/incidents", get(incidents))
        .route("/admin/health/:id/history", get(history))
        .route("/admin/health", post(create))
        .route("/admin/health/:id", post(update).delete(remove))
        .route("/admin/health/:id/toggle", post(toggle))
        .route("/admin/health/:id/check", post(check_now))
}

// Silence "unused import" if `delete` ever stops being referenced via builder.
#[allow(dead_code)]
fn _delete_route_helper(r: Router<AppState>) -> Router<AppState> {
    r.route("/_ignored", delete(|| async { StatusCode::NO_CONTENT }))
}
