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
    ProxyKind::parse(state.config().proxy_kind.as_deref())
}

/// The directory proxy config is written to, if any.
pub fn config_dir(state: &AppState) -> Option<PathBuf> {
    state.config().proxy_config_dir.clone()
}

/// Regenerate every enabled route's config file and reload the proxy.
///
/// When `proxy_config_dir` is unset this is a no-op that still returns a
/// (zeroed) report so the caller can surface "disk output disabled" to the
/// UI.
pub async fn regenerate_all(state: &AppState) -> anyhow::Result<ApplyReport> {
    let mut report = ApplyReport::default();
    let Some(dir) = config_dir(state) else {
        return Ok(report);
    };
    report.dir = Some(dir.display().to_string());

    let kind = configured_kind(state);
    let routes = storage::list_routes(state).await?;

    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        report.errors.push(format!("create_dir_all: {e}"));
        return Ok(report);
    }

    // cloudflared emits a single combined config.yml rather than one file per
    // route, so it takes a separate path.
    if kind.is_single_file() {
        return regenerate_cloudflared(state, &dir, &routes).await;
    }

    // Track files we expect so we can prune stale ones (disabled / deleted).
    let mut expected: std::collections::HashSet<String> = std::collections::HashSet::new();

    for route in &routes {
        if !route.enabled {
            continue;
        }
        let file_name = kind.file_name(&route.subdomain);
        let path = dir.join(&file_name);
        let body = render::render(kind, route, Some(&dir));
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
    prune_stale(&dir, kind, &expected, &mut report).await;

    if let Some(out) = run_reload(state).await {
        report.reload = Some(out);
    }
    Ok(report)
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
        state.config().cloudflared_tunnel.as_deref(),
        state.config().cloudflared_credentials_file.as_deref(),
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
        .proxy_reload_command
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
        Ok(o) if o.status.success() => Some(format!("{cmd} → ok")),
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
            Some(format!("{cmd} → {} :: {detail}", o.status))
        }
        Err(e) => Some(format!("{cmd} → {e}")),
    }
}
