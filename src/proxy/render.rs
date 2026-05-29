//! Proxy config generation.
//!
//! Turns a [`ProxyRoute`] into an nginx `server { … }` block or a Caddyfile
//! site entry.  The emitted text is what gets written to
//! `proxy_config_dir/<subdomain>.conf` and (for nginx) reloaded with the
//! configured reload command.

use super::storage::ProxyRoute;

/// Which proxy syntax to emit.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProxyKind {
    Nginx,
    Caddy,
}

impl ProxyKind {
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("caddy") => ProxyKind::Caddy,
            _ => ProxyKind::Nginx,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProxyKind::Nginx => "nginx",
            ProxyKind::Caddy => "caddy",
        }
    }

    /// File name a single route's config is written to.
    pub fn file_name(self, subdomain: &str) -> String {
        match self {
            ProxyKind::Nginx => format!("{subdomain}.conf"),
            // Caddy imports *.caddy fragments under one Caddyfile.
            ProxyKind::Caddy => format!("{subdomain}.caddy"),
        }
    }
}

/// htpasswd file name for a route (nginx basic-auth).
pub fn htpasswd_file_name(subdomain: &str) -> String {
    format!("{subdomain}.htpasswd")
}

/// Access rules parsed from the route's `access_rules_json`.
#[derive(Debug, Default, serde::Deserialize)]
struct AccessRules {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

fn parse_access(route: &ProxyRoute) -> AccessRules {
    route
        .access_rules_json
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// Render the config fragment for a single route.
///
/// `dir` is the directory the config (and any htpasswd sidecar) is written
/// to — used to emit an absolute `auth_basic_user_file` path for nginx.
pub fn render(kind: ProxyKind, route: &ProxyRoute, dir: Option<&std::path::Path>) -> String {
    match kind {
        ProxyKind::Nginx => render_nginx(route, dir),
        ProxyKind::Caddy => render_caddy(route),
    }
}

fn render_nginx(route: &ProxyRoute, dir: Option<&std::path::Path>) -> String {
    let upstream = format!(
        "{}://{}:{}",
        route.target_scheme, route.target_host, route.target_port
    );
    let access = parse_access(route);
    let mut out = String::new();

    out.push_str(&format!(
        "# Managed by klappstuhl.me — route #{} ({})\n",
        route.id, route.subdomain
    ));
    out.push_str("server {\n");
    if route.ssl_managed {
        out.push_str("    listen 443 ssl;\n");
        out.push_str("    listen [::]:443 ssl;\n");
    } else {
        out.push_str("    listen 80;\n");
        out.push_str("    listen [::]:80;\n");
    }
    out.push_str(&format!("    server_name {};\n", route.subdomain));

    if route.ssl_managed {
        // Conventional certbot path; admins can override via extra_config.
        out.push_str(&format!(
            "    ssl_certificate     /etc/letsencrypt/live/{}/fullchain.pem;\n",
            route.subdomain
        ));
        out.push_str(&format!(
            "    ssl_certificate_key /etc/letsencrypt/live/{}/privkey.pem;\n",
            route.subdomain
        ));
    }

    if route.cloudflare_proxied {
        out.push_str("    real_ip_header CF-Connecting-IP;\n");
        out.push_str("    # set_real_ip_from <cloudflare-ranges> — configure globally\n");
    }

    if let Some(rps) = route.rate_limit_rps {
        // Requires a matching `limit_req_zone` in the http {} block; we
        // reference a conventional zone name keyed on the route id.
        out.push_str(&format!(
            "    # limit_req_zone $binary_remote_addr zone=route{}:10m rate={}r/s; (add to http{{}})\n",
            route.id, rps
        ));
        out.push_str(&format!(
            "    limit_req zone=route{} burst={} nodelay;\n",
            route.id,
            rps.max(1) * 2
        ));
    }

    out.push_str("    location / {\n");
    for cidr in &access.allow {
        out.push_str(&format!("        allow {};\n", cidr));
    }
    for cidr in &access.deny {
        if cidr == "*" {
            out.push_str("        deny all;\n");
        } else {
            out.push_str(&format!("        deny {};\n", cidr));
        }
    }
    if route.has_auth() {
        let htpasswd = htpasswd_file_name(&route.subdomain);
        let path = match dir {
            Some(d) => d.join(&htpasswd).display().to_string(),
            None => htpasswd,
        };
        out.push_str("        auth_basic \"Restricted\";\n");
        out.push_str(&format!("        auth_basic_user_file {};\n", path));
    }
    out.push_str(&format!("        proxy_pass {};\n", upstream));
    out.push_str("        proxy_set_header Host $host;\n");
    out.push_str("        proxy_set_header X-Real-IP $remote_addr;\n");
    out.push_str("        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n");
    out.push_str("        proxy_set_header X-Forwarded-Proto $scheme;\n");
    out.push_str("        proxy_http_version 1.1;\n");
    out.push_str("        proxy_set_header Upgrade $http_upgrade;\n");
    out.push_str("        proxy_set_header Connection \"upgrade\";\n");
    out.push_str("    }\n");

    if let Some(extra) = route.extra_config.as_deref().filter(|s| !s.trim().is_empty()) {
        for line in extra.lines() {
            out.push_str(&format!("    {}\n", line));
        }
    }

    out.push_str("}\n");
    out
}

fn render_caddy(route: &ProxyRoute) -> String {
    let upstream = format!(
        "{}://{}:{}",
        route.target_scheme, route.target_host, route.target_port
    );
    let access = parse_access(route);
    let mut out = String::new();

    out.push_str(&format!(
        "# Managed by klappstuhl.me — route #{} ({})\n",
        route.id, route.subdomain
    ));
    out.push_str(&format!("{} {{\n", route.subdomain));

    if !route.ssl_managed {
        out.push_str("    tls internal\n");
    }

    if !access.allow.is_empty() || !access.deny.is_empty() {
        out.push_str("    @blocked {\n");
        for cidr in &access.deny {
            if cidr == "*" {
                out.push_str("        not remote_ip 0.0.0.0/0\n");
            } else {
                out.push_str(&format!("        remote_ip {}\n", cidr));
            }
        }
        out.push_str("    }\n");
        if !access.allow.is_empty() {
            out.push_str(&format!(
                "    @allowed remote_ip {}\n",
                access.allow.join(" ")
            ));
            out.push_str("    handle @allowed {\n");
        }
        out.push_str("    respond @blocked 403\n");
        if !access.allow.is_empty() {
            out.push_str("    }\n");
        }
    }

    if route.has_auth() {
        if let (Some(user), Some(hash)) = (&route.http_auth_user, &route.http_auth_pass_hash) {
            out.push_str("    basic_auth {\n");
            out.push_str(&format!("        {} {}\n", user, hash));
            out.push_str("    }\n");
        }
    }

    if let Some(rps) = route.rate_limit_rps {
        out.push_str(&format!(
            "    # rate_limit: {} r/s — requires the caddy-ratelimit plugin\n",
            rps
        ));
    }

    out.push_str(&format!("    reverse_proxy {}\n", upstream));

    if let Some(extra) = route.extra_config.as_deref().filter(|s| !s.trim().is_empty()) {
        for line in extra.lines() {
            out.push_str(&format!("    {}\n", line));
        }
    }

    out.push_str("}\n");
    out
}
