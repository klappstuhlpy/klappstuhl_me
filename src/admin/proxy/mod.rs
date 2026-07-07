//! Reverse proxy / domain manager.
//!
//! Maps a subdomain (`jellyfin.klappstuhl.me`) to an upstream
//! container/host:port, then renders an nginx `server { … }` block (or a
//! Caddyfile fragment) and writes it to the configured `proxy_config_dir`.
//! After regenerating, the optional `proxy_reload_command` is run so the
//! proxy picks up the change.
//!
//! Like the firewall module, the DB row is the source of truth: when no
//! `proxy_config_dir` is configured the routes are still managed in the UI
//! (handy as a record of "which subdomain points where") but nothing is
//! written to disk.
pub mod routes; // HTTP handlers for this admin feature (see admin/mod.rs)

pub mod cloudflared;
pub mod render;
pub mod storage;

pub use render::ProxyKind;
pub use storage::{NewRoute, ProxyRoute, RouteView};

use crate::AppState;
use std::path::PathBuf;

/// Result of regenerating proxy config on disk.
#[derive(Debug, Default, serde::Serialize)]
pub struct ApplyReport {
    /// Number of route files written.
    pub written: usize,
    /// Directory the files were written to (None when disk output is off).
    pub dir: Option<String>,
    /// Output / status of the reload command, if one ran.
    pub reload: Option<String>,
    /// Non-fatal errors encountered while writing individual files.
    pub errors: Vec<String>,
}

/// The proxy kind configured for this server.
pub fn configured_kind(state: &AppState) -> ProxyKind {
    ProxyKind::parse(state.config().proxy.kind.as_deref())
}

/// The directory proxy config is written to, if any.
pub fn config_dir(state: &AppState) -> Option<PathBuf> {
    state.config().proxy.config_dir.clone()
}

/// Logs the outcome of an apply so route changes leave a trace even when the
/// caller discards the report (create/update/toggle/delete do).
fn log_apply(backend: &str, report: &ApplyReport) {
    if report.errors.is_empty() {
        tracing::info!(
            backend,
            written = report.written,
            reload = report.reload.as_deref().unwrap_or("-"),
            "proxy config regenerated"
        );
    } else {
        tracing::warn!(
            backend,
            written = report.written,
            errors = report.errors.len(),
            detail = %report.errors.join(" | "),
            reload = report.reload.as_deref().unwrap_or("-"),
            "proxy config regenerated with errors"
        );
    }
}

/// Regenerate proxy config for every enabled route and reload the proxy.
///
/// Three modes:
/// - **cloudflared + API** (`cloudflare_account_id` + `cloudflared_tunnel_id`):
///   push ingress to the Cloudflare API, no local file or reload.
/// - **cloudflared (local)**: write a single `config.yml`, run the reload cmd.
/// - **nginx / caddy**: one file per route, prune stale, run the reload cmd.
///
/// When `proxy_config_dir` is unset (and not in API mode) this is a no-op that
/// still returns a (zeroed) report so the caller can surface "disk output
/// disabled" to the UI.
pub async fn regenerate_all(state: &AppState) -> anyhow::Result<ApplyReport> {
    let kind = configured_kind(state);

    // Cloudflared API mode manages the tunnel over the API — no local dir,
    // file, or reload command. Checked before the proxy_config_dir gate.
    if cloudflared::api_mode(state) {
        let result = cloudflared::push(state).await;
        match &result {
            Ok(report) => log_apply("cloudflared-api", report),
            Err(e) => tracing::warn!(error = %e, "Cloudflare tunnel API push failed"),
        }
        return result;
    }

    let Some(dir) = config_dir(state) else {
        tracing::debug!("proxy_config_dir unset — routes are tracked in the DB only, nothing written");
        return Ok(ApplyReport::default());
    };

    let routes = storage::list_routes(state).await?;

    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        let report = ApplyReport {
            dir: Some(dir.display().to_string()),
            errors: vec![format!("create_dir_all: {e}")],
            ..Default::default()
        };
        tracing::warn!(dir = %dir.display(), error = %e, "could not create proxy config dir");
        return Ok(report);
    }

    // cloudflared (local file) emits a single combined config.yml.
    if kind.is_single_file() {
        let report = regenerate_cloudflared(state, &dir, &routes).await?;
        log_apply("cloudflared", &report);
        return Ok(report);
    }

    let report = regenerate_files(state, &dir, kind, &routes).await;
    log_apply(kind.label(), &report);
    Ok(report)
}

/// nginx / caddy backend: one config file per enabled route, plus prune + reload.
async fn regenerate_files(
    state: &AppState,
    dir: &std::path::Path,
    kind: ProxyKind,
    routes: &[ProxyRoute],
) -> ApplyReport {
    let mut report = ApplyReport {
        dir: Some(dir.display().to_string()),
        ..Default::default()
    };

    // Track files we expect so we can prune stale ones (disabled / deleted).
    let mut expected: std::collections::HashSet<String> = std::collections::HashSet::new();

    for route in routes {
        if !route.enabled {
            continue;
        }
        let file_name = kind.file_name(&route.subdomain);
        let path = dir.join(&file_name);
        let body = render::render(kind, route, Some(dir));
        match tokio::fs::write(&path, body).await {
            Ok(()) => {
                report.written += 1;
                expected.insert(file_name);
            }
            Err(e) => report.errors.push(format!("{}: {e}", path.display())),
        }

        // For nginx, drop an htpasswd sidecar next to the config when the
        // route has basic-auth.  Caddy embeds the hash inline so it needs no
        // sidecar file.
        if matches!(kind, ProxyKind::Nginx) && route.has_auth() {
            if let (Some(user), Some(hash)) = (&route.http_auth_user, &route.http_auth_pass_hash) {
                let ht_name = render::htpasswd_file_name(&route.subdomain);
                let ht_path = dir.join(&ht_name);
                let line = format!("{user}:{hash}\n");
                match tokio::fs::write(&ht_path, line).await {
                    Ok(()) => {
                        expected.insert(ht_name);
                    }
                    Err(e) => report.errors.push(format!("{}: {e}", ht_path.display())),
                }
            }
        }
    }

    // Prune managed files that no longer correspond to an enabled route.
    prune_stale(dir, kind, &expected, &mut report).await;

    if let Some(out) = run_reload(state).await {
        report.reload = Some(out);
    }
    report
}

/// Regenerate the single combined cloudflared `config.yml` from every enabled
/// route, then run the reload command (typically `systemctl restart
/// cloudflared`). Unlike nginx/caddy there is nothing to prune — the one file
/// is rewritten wholesale each time.
async fn regenerate_cloudflared(
    state: &AppState,
    dir: &std::path::Path,
    routes: &[ProxyRoute],
) -> anyhow::Result<ApplyReport> {
    let mut report = ApplyReport {
        dir: Some(dir.display().to_string()),
        ..Default::default()
    };

    let enabled: Vec<&ProxyRoute> = routes.iter().filter(|r| r.enabled).collect();
    let body = render::render_cloudflared_config(
        &enabled,
        state.config().cloudflare.tunnel_name.as_deref(),
        state.config().cloudflare.tunnel_credentials_file.as_deref(),
    );

    let path = dir.join(render::CLOUDFLARED_FILE);
    match tokio::fs::write(&path, body).await {
        Ok(()) => report.written = 1,
        Err(e) => report.errors.push(format!("{}: {e}", path.display())),
    }

    if let Some(out) = run_reload(state).await {
        report.reload = Some(out);
    }
    Ok(report)
}

/// Delete config files we previously generated but that no longer map to an
/// enabled route.  We only touch files matching our naming pattern so we
/// never clobber hand-written config sharing the directory.
async fn prune_stale(
    dir: &std::path::Path,
    kind: ProxyKind,
    expected: &std::collections::HashSet<String>,
    report: &mut ApplyReport,
) {
    let ext = match kind {
        ProxyKind::Nginx => "conf",
        ProxyKind::Caddy => "caddy",
        // Single-file backends have nothing per-route to prune.
        ProxyKind::Cloudflared => return,
    };
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(&format!(".{ext}")) || expected.contains(&name) {
            continue;
        }
        // Only prune files that carry our managed marker on the first line.
        let path = entry.path();
        if let Ok(contents) = tokio::fs::read_to_string(&path).await {
            if contents.starts_with("# Managed by klappstuhl.me") {
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    report.errors.push(format!("prune {}: {e}", path.display()));
                }
            }
        }
    }
}

/// Run the configured reload command (e.g. `nginx -s reload`).  Returns the
/// command line plus a short status string, or `None` when no command is
/// configured.
async fn run_reload(state: &AppState) -> Option<String> {
    let cmd = state
        .config()
        .proxy
        .reload_command
        .as_deref()
        .filter(|s| !s.trim().is_empty())?;

    #[cfg(windows)]
    let mut command = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };

    match command.output().await {
        Ok(o) if o.status.success() => {
            tracing::info!(cmd, "proxy reload command ran");
            Some(format!("{cmd} → ok"))
        }
        Ok(o) => {
            // Prefer stderr for the failure detail, fall back to stdout.
            // `ExitStatus` already Displays as "exit status: N", so don't
            // prefix another "exit".
            let detail = {
                let err = String::from_utf8_lossy(&o.stderr);
                let err = err.trim();
                if err.is_empty() {
                    String::from_utf8_lossy(&o.stdout).trim().to_string()
                } else {
                    err.to_string()
                }
            };
            tracing::warn!(cmd, status = %o.status, detail = %detail, "proxy reload command failed");
            Some(format!("{cmd} → {} :: {detail}", o.status))
        }
        Err(e) => {
            tracing::warn!(cmd, error = %e, "proxy reload command could not be launched");
            Some(format!("{cmd} → {e}"))
        }
    }
}
