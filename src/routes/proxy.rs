//! Reverse proxy / domain manager dashboard.
//!
//! - `GET    /admin/proxy`                 page
//! - `GET    /admin/proxy/data`            routes + proxy kind + container list
//! - `GET    /admin/proxy/:id/preview`     rendered config for one route
//! - `POST   /admin/proxy`                 create a route
//! - `POST   /admin/proxy/:id`             update a route
//! - `POST   /admin/proxy/:id/toggle`      enable/disable a route
//! - `POST   /admin/proxy/apply`           regenerate all config + reload
//! - `DELETE /admin/proxy/:id`             remove a route

use crate::{
    headers::ClientIp,
    models::Account,
    proxy::{self, storage::NewRoute},
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
#[template(path = "admin_proxy.html")]
struct AdminProxyTemplate {
    account: Option<Account>,
    active_page: &'static str,
    proxy_kind: &'static str,
}

async fn page(
    State(state): State<AppState>,
    account: Account,
) -> Result<AdminProxyTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminProxyTemplate {
        account: Some(account),
        active_page: "proxy",
        proxy_kind: proxy::configured_kind(&state).label(),
    })
}

#[derive(Serialize)]
struct ContainerOption {
    name: String,
    identifier: String,
}

#[derive(Serialize)]
struct DashboardData {
    proxy_kind: &'static str,
    config_dir: Option<String>,
    routes: Vec<proxy::RouteView>,
    containers: Vec<ContainerOption>,
    total: i64,
    enabled_count: i64,
}

async fn data(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<DashboardData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let routes = proxy::storage::list_routes(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let enabled_count = routes.iter().filter(|r| r.enabled).count() as i64;
    let total = routes.len() as i64;
    let routes: Vec<proxy::RouteView> = routes.into_iter().map(Into::into).collect();

    // Offer configured Docker services as target options ("→ container: …").
    let containers = state
        .config()
        .services
        .iter()
        .map(|s| ContainerOption {
            name: s.name.clone(),
            identifier: s.identifier.clone(),
        })
        .collect();

    Ok(Json(DashboardData {
        proxy_kind: proxy::configured_kind(&state).label(),
        config_dir: proxy::config_dir(&state).map(|p| p.display().to_string()),
        routes,
        containers,
        total,
        enabled_count,
    }))
}

async fn preview(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let route = proxy::storage::get_route(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let kind = proxy::configured_kind(&state);
    let dir = proxy::config_dir(&state);
    let config = proxy::render::render(kind, &route, dir.as_deref());
    Ok(Json(serde_json::json!({
        "kind": kind.label(),
        "file": kind.file_name(&route.subdomain),
        "config": config,
    })))
}

#[derive(Deserialize)]
struct UpsertForm {
    subdomain: String,
    target_host: String,
    target_port: i64,
    #[serde(default)]
    target_scheme: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    ssl_managed: Option<String>,
    #[serde(default)]
    cloudflare_proxied: Option<String>,
    #[serde(default)]
    http_auth_user: Option<String>,
    /// Plaintext password — hashed (bcrypt) before storage.  Empty on edit
    /// means "keep the existing credential".
    #[serde(default)]
    http_auth_password: Option<String>,
    #[serde(default)]
    rate_limit_rps: Option<i64>,
    #[serde(default)]
    access_rules_json: Option<String>,
    #[serde(default)]
    extra_config: Option<String>,
    #[serde(default)]
    enabled: Option<String>,
}

fn checkbox(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("on" | "true" | "1"))
}

impl UpsertForm {
    fn validate(self) -> Result<NewRoute, StatusCode> {
        let subdomain = self.subdomain.trim().to_ascii_lowercase();
        let target_host = self.target_host.trim().to_string();
        if subdomain.is_empty() || target_host.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        // Cheap hostname sanity check: no scheme, no path, no spaces.
        if subdomain.contains("://") || subdomain.contains('/') || subdomain.contains(' ') {
            return Err(StatusCode::BAD_REQUEST);
        }
        if !(1..=65535).contains(&self.target_port) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let target_scheme = match self.target_scheme.as_deref() {
            Some("https") => "https".to_string(),
            _ => "http".to_string(),
        };
        let container = self.container.filter(|c| !c.trim().is_empty());
        let http_auth_user = self
            .http_auth_user
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());
        // Hash the password only when one was provided.
        let http_auth_pass_hash = match self
            .http_auth_password
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(pw) => Some(
                bcrypt::hash(pw, bcrypt::DEFAULT_COST)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            ),
            None => None,
        };
        // Validate access rules JSON shape if present.
        let access_rules_json = self
            .access_rules_json
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                serde_json::from_str::<serde_json::Value>(&s)
                    .map(|_| s)
                    .map_err(|_| StatusCode::BAD_REQUEST)
            })
            .transpose()?;
        let rate_limit_rps = self.rate_limit_rps.filter(|r| *r > 0);
        let extra_config = self.extra_config.filter(|s| !s.trim().is_empty());

        Ok(NewRoute {
            subdomain,
            target_host,
            target_port: self.target_port,
            target_scheme,
            container,
            ssl_managed: checkbox(&self.ssl_managed),
            cloudflare_proxied: checkbox(&self.cloudflare_proxied),
            http_auth_user,
            http_auth_pass_hash,
            rate_limit_rps,
            access_rules_json,
            extra_config,
            enabled: self.enabled.is_none() || checkbox(&self.enabled),
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
    let route = form.validate()?;
    let subdomain = route.subdomain.clone();
    let id = proxy::storage::create_route(&state, route)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    let report = proxy::regenerate_all(&state).await.ok();
    state
        .audit("proxy.route.create")
        .actor(&account)
        .target(format!("proxy:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "subdomain": subdomain }))
        .fire();
    Ok(Json(serde_json::json!({ "id": id, "apply": report })))
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
    let route = form.validate()?;
    let subdomain = route.subdomain.clone();
    proxy::storage::update_route(&state, id, route)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    let _ = proxy::regenerate_all(&state).await;
    state
        .audit("proxy.route.update")
        .actor(&account)
        .target(format!("proxy:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "subdomain": subdomain }))
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
    proxy::storage::delete_route(&state, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = proxy::regenerate_all(&state).await;
    state
        .audit("proxy.route.delete")
        .actor(&account)
        .target(format!("proxy:{id}"))
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
    proxy::storage::set_enabled(&state, id, enabled)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let _ = proxy::regenerate_all(&state).await;
    state
        .audit("proxy.route.toggle")
        .actor(&account)
        .target(format!("proxy:{id}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "enabled": enabled }))
        .fire();
    Ok(StatusCode::NO_CONTENT)
}

async fn apply(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
) -> Result<Json<proxy::ApplyReport>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let report = proxy::regenerate_all(&state)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .audit("proxy.apply")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "written": report.written,
            "errors": report.errors.len(),
        }))
        .fire();
    Ok(Json(report))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/proxy", get(page).post(create))
        .route("/admin/proxy/data", get(data))
        .route("/admin/proxy/apply", post(apply))
        .route("/admin/proxy/:id", post(update).delete(remove))
        .route("/admin/proxy/:id/preview", get(preview))
        .route("/admin/proxy/:id/toggle", post(toggle))
}
