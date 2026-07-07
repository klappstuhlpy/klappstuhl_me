//! Firewall management dashboard.
//!
//! - `GET    /admin/firewall`               page
//! - `GET    /admin/firewall/data`          rules + lockouts + backend status
//! - `POST   /admin/firewall/rule`          create a rule
//! - `POST   /admin/firewall/rule/:id/toggle` enable/disable a rule
//! - `DELETE /admin/firewall/rule/:id`      remove a rule
//! - `POST   /admin/firewall/lockout`       manually block an IP
//! - `POST   /admin/firewall/lockout/:id/release` release an active lockout
//! - `POST   /admin/firewall/apply`         re-apply all rules to backend

use crate::{
    firewall::{self, backend::Backend, storage::NewRule},
    headers::ClientIp,
    models::Account,
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Form, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Template)]
#[template(path = "admin/admin_firewall.html")]
struct AdminFirewallTemplate {
    account: Option<Account>,
    active_page: &'static str,
    backend_label: &'static str,
}

async fn page(State(state): State<AppState>, account: Account) -> Result<AdminFirewallTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let backend_label = state.firewall_backend().map(|b| b.kind.label()).unwrap_or("disabled");
    Ok(AdminFirewallTemplate {
        account: Some(account),
        active_page: "firewall",
        backend_label,
    })
}

#[derive(Serialize)]
struct DashboardData {
    backend: &'static str,
    rules: Vec<firewall::FirewallRule>,
    lockouts: Vec<firewall::LockoutRow>,
    auto_threshold: i64,
    auto_window_secs: i64,
    auto_lockout_secs: i64,
}

async fn data(State(state): State<AppState>, account: Account) -> Result<Json<DashboardData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let backend = state.firewall_backend().map(|b| b.kind.label()).unwrap_or("disabled");
    // Reconcile the live ufw ruleset into the mirror so rules created
    // out-of-band still show up. Best-effort; never blocks the dashboard.
    firewall::sync::sync_live(&state).await;
    let rules = firewall::storage::list_rules(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let lockouts = firewall::storage::list_lockouts(&state, false)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(DashboardData {
        backend,
        rules,
        lockouts,
        auto_threshold: firewall::lockout::DEFAULT_THRESHOLD,
        auto_window_secs: firewall::lockout::DEFAULT_WINDOW_SECS,
        auto_lockout_secs: firewall::lockout::DEFAULT_LOCKOUT_SECS,
    }))
}

#[derive(Deserialize)]
struct RuleForm {
    action: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    proto: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    port: Option<i64>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    rate_per_s: Option<i64>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    enabled: Option<String>,
}

impl RuleForm {
    fn validate(self) -> Result<NewRule, StatusCode> {
        let action = self.action.trim().to_string();
        if !matches!(action.as_str(), "allow" | "deny" | "rate_limit" | "geo_block") {
            return Err(StatusCode::BAD_REQUEST);
        }
        let direction = self.direction.unwrap_or_else(|| "in".to_string());
        if !matches!(direction.as_str(), "in" | "out" | "any") {
            return Err(StatusCode::BAD_REQUEST);
        }
        let proto = self.proto.unwrap_or_else(|| "any".to_string());
        if !matches!(proto.as_str(), "tcp" | "udp" | "icmp" | "any") {
            return Err(StatusCode::BAD_REQUEST);
        }
        let source = self.source.filter(|s| !s.trim().is_empty());
        let country = self
            .country
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty());
        if action == "geo_block" && country.is_none() {
            return Err(StatusCode::BAD_REQUEST);
        }
        let enabled = !matches!(self.enabled.as_deref(), Some("false" | "0" | "off"));
        Ok(NewRule {
            action,
            direction,
            proto,
            source,
            port: self.port,
            country,
            rate_per_s: self.rate_per_s,
            note: self.note,
            enabled,
        })
    }
}

async fn create_rule(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Form(form): Form<RuleForm>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let rule = form.validate()?;
    let action = rule.action.clone();
    let source = rule.source.clone();
    let id = firewall::storage::create_rule(&state, rule)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Best-effort apply.  Failures are logged but don't block the response
    // because the DB row is still the source of truth and the admin can
    // re-apply manually.
    let mut apply_output: Option<String> = None;
    if let Some(rule) = firewall::storage::get_rule(&state, id).await.ok().flatten() {
        if rule.enabled {
            if let Some(backend) = state.firewall_backend() {
                if let Some(argv) = backend.apply_command(&rule) {
                    let preview = Backend::render(&argv);
                    match backend.exec(argv).await {
                        Ok(o) if o.status.success() => apply_output = Some(preview),
                        Ok(o) => {
                            apply_output = Some(format!(
                                "{preview} → exit {} :: {}",
                                o.status,
                                String::from_utf8_lossy(&o.stderr)
                            ));
                        }
                        Err(e) => apply_output = Some(format!("{preview} → {e}")),
                    }
                }
            }
        }
    }
    state
        .audit("firewall.rule.create")
        .actor(&account)
        .target(format!("firewall:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "action": action,
            "source": source,
            "apply": apply_output,
        }))
        .fire();
    Ok(Json(serde_json::json!({ "id": id, "apply": apply_output })))
}

async fn toggle_rule(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
    Form(form): Form<TogglePayload>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let enabled = matches!(form.enabled.as_str(), "true" | "1" | "on");
    firewall::storage::toggle_rule(&state, id, enabled)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(rule) = firewall::storage::get_rule(&state, id).await.ok().flatten() {
        if let Some(backend) = state.firewall_backend() {
            let argv = if enabled {
                backend.apply_command(&rule)
            } else {
                backend.remove_command(&rule)
            };
            if let Some(argv) = argv {
                let _ = backend.exec(argv).await;
            }
        }
    }
    state
        .audit("firewall.rule.toggle")
        .actor(&account)
        .target(format!("firewall:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "enabled": enabled }))
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TogglePayload {
    enabled: String,
}

async fn delete_rule(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    // Remove from backend first; ignore failure.
    if let Some(rule) = firewall::storage::get_rule(&state, id).await.ok().flatten() {
        if let Some(backend) = state.firewall_backend() {
            if let Some(argv) = backend.remove_command(&rule) {
                let _ = backend.exec(argv).await;
            }
        }
    }
    firewall::storage::delete_rule(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("firewall.rule.delete")
        .actor(&account)
        .target(format!("firewall:{id}"))
        .ip_opt(client_ip)
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct LockoutForm {
    ip: String,
    #[serde(default)]
    reason: Option<String>,
    /// Lockout length in seconds.  Empty / 0 = indefinite.
    #[serde(default)]
    duration_secs: Option<i64>,
}

async fn add_lockout(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Form(form): Form<LockoutForm>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    if form.ip.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let duration = form.duration_secs.filter(|s| *s > 0);
    let reason = form.reason.unwrap_or_else(|| "manual".to_string());
    let id = firewall::storage::add_lockout(&state, form.ip.trim(), &reason, duration)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(backend) = state.firewall_backend() {
        if let Some(argv) = backend.lockout_command(form.ip.trim(), true) {
            let _ = backend.exec(argv).await;
        }
    }
    state
        .audit("firewall.lockout.add")
        .actor(&account)
        .target(form.ip.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "reason": reason, "duration_secs": duration }))
        .fire();
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn release_lockout(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    // Look up IP first so we can remove the kernel rule.
    let target_ip = firewall::storage::list_lockouts(&state, false)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .find(|l| l.id == id)
        .map(|l| l.ip);
    firewall::storage::release_lockout(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let (Some(ip), Some(backend)) = (target_ip.as_deref(), state.firewall_backend()) {
        if let Some(argv) = backend.lockout_command(ip, false) {
            let _ = backend.exec(argv).await;
        }
    }
    state
        .audit("firewall.lockout.release")
        .actor(&account)
        .target(target_ip.unwrap_or_else(|| format!("lockout:{id}")))
        .ip_opt(client_ip)
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

async fn reapply_all(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let backend = match state.firewall_backend() {
        Some(b) => b,
        None => {
            return Ok(Json(serde_json::json!({
                "applied": 0,
                "skipped": "no backend configured",
            })));
        }
    };
    let rules = firewall::storage::list_rules(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut applied = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for rule in rules {
        if !rule.enabled {
            continue;
        }
        let Some(argv) = backend.apply_command(&rule) else {
            continue;
        };
        let preview = Backend::render(&argv);
        match backend.exec(argv).await {
            Ok(o) if o.status.success() => applied += 1,
            Ok(o) => errors.push(format!(
                "{preview} → exit {} :: {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => errors.push(format!("{preview} → {e}")),
        }
    }
    state
        .audit("firewall.apply_all")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "applied": applied, "errors": errors.len() }))
        .fire();
    Ok(Json(serde_json::json!({
        "applied": applied,
        "errors": errors,
    })))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/firewall", get(page))
        .route("/admin/firewall/data", get(data))
        .route("/admin/firewall/rule", post(create_rule))
        .route("/admin/firewall/rule/:id", axum::routing::delete(delete_rule))
        .route("/admin/firewall/rule/:id/toggle", post(toggle_rule))
        .route("/admin/firewall/lockout", post(add_lockout))
        .route("/admin/firewall/lockout/:id/release", post(release_lockout))
        .route("/admin/firewall/apply", post(reapply_all))
}
