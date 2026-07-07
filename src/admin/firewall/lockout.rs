//! Automatic lockout helpers.
//!
//! Triggered from the auth handler when a login fails: after N failures
//! in a short window the IP is locked out and the configured firewall
//! backend is asked to drop traffic from that source.

use crate::AppState;

/// How many failed logins within the window trigger a lockout.
pub const DEFAULT_THRESHOLD: i64 = 8;
/// Lockout window for the failure count.
pub const DEFAULT_WINDOW_SECS: i64 = 600;
/// How long a single lockout lasts.
pub const DEFAULT_LOCKOUT_SECS: i64 = 60 * 60;

/// Called from the audit pipeline (`auth.login.fail`).  Counts recent
/// failures from this IP and triggers a lockout when the threshold is
/// crossed.
pub async fn register_failure(state: &AppState, ip: &str) -> anyhow::Result<bool> {
    if ip.is_empty() {
        return Ok(false);
    }
    if super::storage::find_active_lockout(state, ip).await?.is_some() {
        return Ok(false);
    }
    let recent = recent_failure_count(state, ip, DEFAULT_WINDOW_SECS).await?;
    if recent < DEFAULT_THRESHOLD {
        return Ok(false);
    }
    super::storage::add_lockout(
        state,
        ip,
        &format!("auto: {recent} failed logins in {DEFAULT_WINDOW_SECS}s"),
        Some(DEFAULT_LOCKOUT_SECS),
    )
    .await?;
    apply_backend_block(state, ip, true).await;
    state
        .audit("firewall.lockout.auto")
        .actor_label("system")
        .target(ip.to_string())
        .meta(serde_json::json!({ "recent_failures": recent }))
        .fire();
    Ok(true)
}

async fn recent_failure_count(state: &AppState, ip: &str, window_secs: i64) -> rusqlite::Result<i64> {
    let ip = ip.to_string();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<i64> {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log
                 WHERE action = 'auth.login.fail'
                   AND ip = ?
                   AND ts >= datetime('now', ?)",
                rusqlite::params![ip, format!("-{window_secs} seconds")],
                |r| r.get(0),
            )
        })
        .await
}

pub async fn reap_expired(state: &AppState) -> anyhow::Result<usize> {
    let released = super::storage::release_expired(state).await?;
    let n = released.len();
    for ip in released {
        apply_backend_block(state, &ip, false).await;
    }
    Ok(n)
}

async fn apply_backend_block(state: &AppState, ip: &str, add: bool) {
    let Some(backend) = state.firewall_backend() else {
        return;
    };
    let Some(argv) = backend.lockout_command(ip, add) else {
        return;
    };
    if let Err(e) = backend.exec(argv.clone()).await {
        tracing::warn!(error = %e, ?argv, "firewall backend exec failed");
    }
}
