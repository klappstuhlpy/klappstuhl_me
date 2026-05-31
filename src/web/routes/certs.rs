//! Cert & domain overview — a read-only page that joins managed reverse-proxy
//! routes with the SSL health monitors, so every domain the box serves is
//! visible in one place alongside its certificate expiry.
//!
//! It owns no state of its own: routes come from `/admin/proxy` and cert data
//! from the `ssl` health monitors on `/admin/health`. This is purely a
//! convenience aggregation view (`GET /admin/certs`).

use crate::{health, models::Account, proxy, AppState};
use askama::Template;
use axum::{extract::State, http::StatusCode, routing::get, Router};

/// One proxy route joined with its matching SSL monitor (if any).
struct RouteCertView {
    subdomain: String,
    upstream: String,
    container: Option<String>,
    ssl_managed: bool,
    cloudflare_proxied: bool,
    has_auth: bool,
    enabled: bool,
    /// TLS terminates at Cloudflare's edge (cloudflared backend, or a
    /// cloudflare-proxied route), so there is no local certificate to track.
    edge_tls: bool,
    /// Days until the certificate expires, from a matching `ssl` monitor.
    ssl_days_left: Option<i64>,
    /// Name of the matched monitor, for a link back to `/admin/health`.
    monitor_name: Option<String>,
}

/// A standalone SSL monitor with no corresponding proxy route.
struct StandaloneCertView {
    name: String,
    host: String,
    ssl_days_left: Option<i64>,
    status: Option<String>,
    uptime_24h: f64,
}

#[derive(Template)]
#[template(path = "admin/admin_certs.html")]
struct AdminCertsTemplate {
    account: Option<Account>,
    active_page: &'static str,
    routes: Vec<RouteCertView>,
    standalone: Vec<StandaloneCertView>,
}

/// Extracts the bare lowercase host from a monitor target that may be a URL
/// (`https://x/y`), a `host:port` pair, or a plain hostname.
fn host_of(target: &str) -> String {
    let t = target.trim();
    let t = t.split("://").nth(1).unwrap_or(t); // strip scheme
    let t = t.split('/').next().unwrap_or(t); // strip path
    let t = t.split('@').next_back().unwrap_or(t); // strip any userinfo
    t.split(':').next().unwrap_or(t).trim().to_ascii_lowercase() // strip port
}

/// Classifies remaining certificate lifetime for the badge colour.
/// Exposed to the template as a string class.
fn cert_class(days: Option<i64>) -> &'static str {
    match days {
        Some(d) if d <= 7 => "danger",
        Some(d) if d <= 21 => "warn",
        Some(_) => "ok",
        None => "unknown",
    }
}

async fn page(State(state): State<AppState>, account: Account) -> Result<AdminCertsTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let proxy_routes = proxy::storage::list_routes(&state).await.unwrap_or_default();
    let summaries = health::storage::list_summaries(&state).await.unwrap_or_default();

    // With a cloudflared backend every route is fronted by Cloudflare's edge,
    // which manages TLS — so no local certificate exists to monitor.
    let backend_is_edge = proxy::configured_kind(&state).label() == "cloudflared";

    // SSL monitors keyed by bare host, for the join.
    let ssl_monitors: Vec<&_> = summaries.iter().filter(|s| s.target.kind.eq_ignore_ascii_case("ssl")).collect();

    let mut matched_monitor_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut routes = Vec::with_capacity(proxy_routes.len());
    for r in &proxy_routes {
        let want = r.subdomain.to_ascii_lowercase();
        let monitor = ssl_monitors.iter().find(|m| host_of(&m.target.target) == want);
        if let Some(m) = monitor {
            matched_monitor_ids.insert(m.target.id);
        }
        routes.push(RouteCertView {
            subdomain: r.subdomain.clone(),
            upstream: format!("{}://{}:{}", r.target_scheme, r.target_host, r.target_port),
            container: r.container.clone(),
            ssl_managed: r.ssl_managed,
            cloudflare_proxied: r.cloudflare_proxied,
            has_auth: r.has_auth(),
            enabled: r.enabled,
            edge_tls: backend_is_edge || r.cloudflare_proxied,
            ssl_days_left: monitor.and_then(|m| m.last_ssl_days_left),
            monitor_name: monitor.map(|m| m.target.name.clone()),
        });
    }

    // SSL monitors that didn't line up with any proxy route.
    let standalone = ssl_monitors
        .iter()
        .filter(|m| !matched_monitor_ids.contains(&m.target.id))
        .map(|m| StandaloneCertView {
            name: m.target.name.clone(),
            host: host_of(&m.target.target),
            ssl_days_left: m.last_ssl_days_left,
            status: m.last_status.clone(),
            uptime_24h: m.uptime_24h,
        })
        .collect();

    Ok(AdminCertsTemplate {
        account: Some(account),
        active_page: "certs",
        routes,
        standalone,
    })
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/certs", get(page))
}

// Template-facing helpers.
impl RouteCertView {
    fn cert_class(&self) -> &'static str {
        cert_class(self.ssl_days_left)
    }
}
impl StandaloneCertView {
    fn cert_class(&self) -> &'static str {
        cert_class(self.ssl_days_left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_handles_url_port_and_plain() {
        assert_eq!(host_of("https://jellyfin.example.com/health"), "jellyfin.example.com");
        assert_eq!(host_of("jellyfin.example.com:8920"), "jellyfin.example.com");
        assert_eq!(host_of("EXAMPLE.com"), "example.com");
    }

    #[test]
    fn cert_class_thresholds() {
        assert_eq!(cert_class(Some(3)), "danger");
        assert_eq!(cert_class(Some(14)), "warn");
        assert_eq!(cert_class(Some(60)), "ok");
        assert_eq!(cert_class(None), "unknown");
    }
}
