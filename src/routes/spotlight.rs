//! Spotlight (Ctrl+K) backend routes.
//!
//! GET  /admin/spotlight/search?q=  — fuzzy search across all browseable routes,
//!                                    API endpoints, images, audit log, containers,
//!                                    file scans, SSH keys, and static nav items
//! POST /admin/spotlight/run        — execute a pre-defined script from config
//! GET  /admin/spotlight/scripts    — list configured scripts (for the palette)

use std::time::Duration;

use crate::{models::Account, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};

// ─── Result types ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SpotlightItem {
    kind: &'static str,
    title: String,
    subtitle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    script_id: Option<String>,
}

impl SpotlightItem {
    fn nav(title: impl Into<String>, subtitle: impl Into<String>, url: impl Into<String>) -> Self {
        Self { kind: "navigate", title: title.into(), subtitle: subtitle.into(),
               url: Some(url.into()), script_id: None }
    }
    fn page(title: impl Into<String>, subtitle: impl Into<String>, url: impl Into<String>) -> Self {
        Self { kind: "page", title: title.into(), subtitle: subtitle.into(),
               url: Some(url.into()), script_id: None }
    }
    fn result(kind: &'static str, title: impl Into<String>, subtitle: impl Into<String>, url: impl Into<String>) -> Self {
        Self { kind, title: title.into(), subtitle: subtitle.into(),
               url: Some(url.into()), script_id: None }
    }
    fn script(title: impl Into<String>, subtitle: impl Into<String>, id: impl Into<String>) -> Self {
        Self { kind: "script", title: title.into(), subtitle: subtitle.into(),
               url: None, script_id: Some(id.into()) }
    }
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

// ─── Admin nav items ──────────────────────────────────────────────────────────

fn static_nav() -> Vec<SpotlightItem> {
    vec![
        SpotlightItem::nav("Dashboard",        "Admin overview",                      "/admin"),
        SpotlightItem::nav("Invites",          "Manage invite codes",                 "/admin/invites"),
        SpotlightItem::nav("Docker",           "Services, graph, start/stop/restart", "/admin/docker"),
        SpotlightItem::nav("Snapshots",        "Capture and restore containers",      "/admin/docker/snapshots"),
        SpotlightItem::nav("Proxy",            "Reverse-proxy route mapping",         "/admin/proxy"),
        SpotlightItem::nav("Metrics",          "CPU, memory, network charts",         "/admin/metrics"),
        SpotlightItem::nav("Health",           "Uptime monitors and incidents",       "/admin/health"),
        SpotlightItem::nav("Security",         "Requests, GeoIP, Cloudflare",         "/admin/security"),
        SpotlightItem::nav("Firewall",         "Packet-filter rules and lockouts",    "/admin/firewall"),
        SpotlightItem::nav("Secrets",          "Secret scanner findings",             "/admin/secrets"),
        SpotlightItem::nav("Audit log",        "All admin actions",                   "/admin/audit"),
        SpotlightItem::nav("Postgres",         "Query the database",                  "/admin/postgres"),
        SpotlightItem::nav("SSH Keys",         "Keys, tokens, session audit",         "/admin/ssh"),
        SpotlightItem::nav("SSH Session Audit","Active sessions and logins",          "/admin/ssh/audit"),
        SpotlightItem::nav("File Sanitizer",   "ClamAV + VirusTotal scanning",        "/admin/sanitizer"),
    ]
}

// ─── Site pages (all browseable non-admin routes) ─────────────────────────────

fn static_pages() -> Vec<SpotlightItem> {
    vec![
        SpotlightItem::page("Home",         "Landing page",                    "/"),
        SpotlightItem::page("Projects",     "Project portfolio",               "/projects"),
        SpotlightItem::page("Images",       "Uploaded image gallery",          "/images"),
        SpotlightItem::page("Account",      "Profile, API tokens, settings",   "/account"),
        SpotlightItem::page("API Docs",     "OpenAPI 3.0 reference (Scalar)",  "/api/docs"),
        SpotlightItem::page("Login",        "Sign in to your account",         "/login"),
    ]
}

// ─── API endpoints (all documented at /api/docs) ──────────────────────────────

fn static_api() -> Vec<SpotlightItem> {
    vec![
        SpotlightItem::result(
            "api",
            "File Scan API",
            "POST /api/scan · malware scan via ClamAV + VirusTotal",
            "/api/docs",
        ),
        SpotlightItem::result(
            "api",
            "Image Effects API",
            "POST /api/image/:op · blur, pixelate, deepfry, invert, grayscale",
            "/api/docs",
        ),
        SpotlightItem::result(
            "api",
            "Image Convert API",
            "POST /api/convert · PNG→WebP, JPEG, GIF, BMP, TIFF",
            "/api/docs",
        ),
        SpotlightItem::result(
            "api",
            "Image Upload / Download API",
            "POST /api/images/upload · /api/images/download · DELETE /api/images/:id",
            "/api/docs",
        ),
    ]
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Human-readable file size (B / KB / MB).
fn fmt_size(bytes: i64) -> String {
    if bytes < 1_024 {
        return format!("{bytes} B");
    }
    if bytes < 1_048_576 {
        return format!("{:.1} KB", bytes as f64 / 1_024.0);
    }
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

/// Returns the subtype of a MIME string in upper-case (e.g. `"image/png"` → `"PNG"`).
fn mime_short(mime: &str) -> String {
    mime.split('/')
        .nth(1)
        .unwrap_or(mime)
        .split('+')     // e.g. "svg+xml" → "SVG"
        .next()
        .unwrap_or(mime)
        .to_uppercase()
}

// ─── Search endpoint ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

async fn search(
    State(state): State<AppState>,
    account: Account,
    Query(params): Query<SearchQuery>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let q = params.q.trim().to_owned();
    let mut items: Vec<SpotlightItem> = Vec::new();

    // ── Admin nav ─────────────────────────────────────────────────────────────
    for item in static_nav() {
        if q.is_empty()
            || contains_ci(&item.title, &q)
            || contains_ci(&item.subtitle, &q)
        {
            items.push(item);
        }
        if items.len() >= 6 && !q.is_empty() { break; }
    }

    // ── Site pages ────────────────────────────────────────────────────────────
    for item in static_pages() {
        if q.is_empty()
            || contains_ci(&item.title, &q)
            || contains_ci(&item.subtitle, &q)
        {
            items.push(item);
        }
    }

    // ── API endpoints ───────────────────────────────────────────────────────────
    for item in static_api() {
        if q.is_empty()
            || contains_ci(&item.title, &q)
            || contains_ci(&item.subtitle, &q)
        {
            items.push(item);
        }
    }

    if q.is_empty() {
        // For empty query return nav + site pages + API + scripts — no DB queries.
        append_scripts(&state, &q, &mut items);
        return Json(serde_json::json!({ "items": items })).into_response();
    }

    // ── Scripts ───────────────────────────────────────────────────────────────
    append_scripts(&state, &q, &mut items);

    // ── Images ────────────────────────────────────────────────────────────────
    let like = format!("%{q}%");
    if let Ok(rows) = state
        .database()
        .call({
            let like = like.clone();
            move |conn| -> rusqlite::Result<Vec<(String, String, i64)>> {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, mimetype, size FROM images
                     WHERE id LIKE ?1 OR mimetype LIKE ?1
                     ORDER BY uploaded_at DESC LIMIT 5",
                )?;
                let rows = stmt.query_map([&like], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                })?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        })
        .await
    {
        for (id, mimetype, size) in rows {
            let subtitle = format!("{} · {}", mime_short(&mimetype), fmt_size(size));
            items.push(SpotlightItem::result("image", id.clone(), subtitle, format!("/gallery/{id}")));
        }
    }

    // ── Audit log ─────────────────────────────────────────────────────────────
    if let Ok(rows) = state
        .database()
        .call({
            let like = like.clone();
            move |conn| -> rusqlite::Result<Vec<(String, String, String)>> {
                let mut stmt = conn.prepare_cached(
                    "SELECT action, actor_label, COALESCE(target,'') FROM audit_log
                     WHERE action LIKE ?1 OR actor_label LIKE ?1 OR target LIKE ?1
                     ORDER BY id DESC LIMIT 5",
                )?;
                let rows = stmt.query_map([&like], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                })?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        })
        .await
    {
        for (action, actor, target) in rows {
            let subtitle = if target.is_empty() {
                format!("by {actor}")
            } else {
                format!("by {actor} → {target}")
            };
            items.push(SpotlightItem::result("audit", action, subtitle, "/admin/audit"));
        }
    }

    // ── File scans ────────────────────────────────────────────────────────────
    if let Ok(rows) = state
        .database()
        .call({
            let like = like.clone();
            move |conn| -> rusqlite::Result<Vec<(String, String)>> {
                let mut stmt = conn.prepare_cached(
                    "SELECT filename, sha256 FROM file_scan
                     WHERE filename LIKE ?1 OR sha256 LIKE ?1
                     ORDER BY id DESC LIMIT 3",
                )?;
                let rows = stmt.query_map([&like], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        })
        .await
    {
        for (filename, sha256) in rows {
            let short = &sha256[..16.min(sha256.len())];
            items.push(SpotlightItem::result(
                "scan",
                filename,
                format!("SHA-256 {short}…"),
                "/admin/sanitizer",
            ));
        }
    }

    // ── SSH keys ──────────────────────────────────────────────────────────────
    if let Ok(rows) = state
        .database()
        .call({
            let like = like.clone();
            move |conn| -> rusqlite::Result<Vec<(String, String)>> {
                let mut stmt = conn.prepare_cached(
                    "SELECT name, fingerprint FROM ssh_key
                     WHERE name LIKE ?1 OR fingerprint LIKE ?1
                     ORDER BY id DESC LIMIT 3",
                )?;
                let rows = stmt.query_map([&like], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        })
        .await
    {
        for (name, fp) in rows {
            items.push(SpotlightItem::result("ssh", name, fp, "/admin/ssh"));
        }
    }

    // ── Docker containers ─────────────────────────────────────────────────────
    if let Some(docker) = state.docker() {
        if let Ok(containers) = docker.containers().await {
            for c in containers.iter().take(5) {
                let cname = c.names.as_ref()
                    .and_then(|n| n.first())
                    .map(|n| n.trim_start_matches('/').to_owned())
                    .unwrap_or_default();
                let image = c.image.clone().unwrap_or_default();
                if contains_ci(&cname, &q) || contains_ci(&image, &q) {
                    let state_str = c.state.clone().unwrap_or_default();
                    items.push(SpotlightItem::result(
                        "container",
                        cname,
                        format!("{image} · {state_str}"),
                        "/admin/docker",
                    ));
                }
            }
        }
    }

    Json(serde_json::json!({ "items": items })).into_response()
}

fn append_scripts(state: &AppState, q: &str, items: &mut Vec<SpotlightItem>) {
    for s in &state.config().spotlight_scripts {
        if q.is_empty()
            || contains_ci(&s.name, q)
            || s.description.as_deref().map(|d| contains_ci(d, q)).unwrap_or(false)
        {
            let subtitle = s.description.clone().unwrap_or_else(|| s.command.clone());
            items.push(SpotlightItem::script(s.name.clone(), subtitle, s.id.clone()));
        }
    }
}

// ─── Script list ──────────────────────────────────────────────────────────────

async fn scripts_list(State(state): State<AppState>, account: Account) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let scripts: Vec<serde_json::Value> = state
        .config()
        .spotlight_scripts
        .iter()
        .map(|s| serde_json::json!({
            "id": s.id,
            "name": s.name,
            "description": s.description,
        }))
        .collect();
    Json(serde_json::json!({ "scripts": scripts })).into_response()
}

// ─── Script runner ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RunPayload {
    script_id: String,
}

async fn run_script(
    State(state): State<AppState>,
    account: Account,
    Json(payload): Json<RunPayload>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let script = state
        .config()
        .spotlight_scripts
        .iter()
        .find(|s| s.id == payload.script_id)
        .cloned();

    let Some(script) = script else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "unknown script id" }))).into_response();
    };

    let mut cmd = build_command(&script.command);
    if let Some(cwd) = &script.cwd {
        cmd.current_dir(cwd);
    }

    let result = tokio::time::timeout(Duration::from_secs(30), cmd.output()).await;

    let output = match result {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() }))).into_response();
        }
        Err(_) => {
            return (StatusCode::GATEWAY_TIMEOUT,
                Json(serde_json::json!({ "error": "script timed out (30s)" }))).into_response();
        }
    };

    state.audit("spotlight.script.run")
        .actor(&account)
        .target(script.name.clone())
        .fire();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Json(serde_json::json!({
        "exit_code": output.status.code(),
        "success":   output.status.success(),
        "stdout":    stdout,
        "stderr":    stderr,
    })).into_response()
}

#[cfg(unix)]
fn build_command(command: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    // systemd units often launch with a stripped PATH, which makes
    // `sh -c "curl …"` fail with "curl: not found" even when curl is
    // installed in /usr/bin. Make sure the standard system locations
    // are searched, while still honouring whatever the parent set.
    let path = match std::env::var_os("PATH") {
        Some(p) if !p.is_empty() => {
            let mut combined = p.into_string().unwrap_or_default();
            combined.push_str(":/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
            combined
        }
        _ => "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
    };
    cmd.env("PATH", path);
    cmd
}

#[cfg(not(unix))]
fn build_command(command: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("cmd");
    cmd.args(["/C", command]);
    cmd
}

// ─── Router ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/spotlight/search", get(search))
        .route("/admin/spotlight/scripts", get(scripts_list))
        .route("/admin/spotlight/run", post(run_script))
}
