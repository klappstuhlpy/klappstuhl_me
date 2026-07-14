//! Secret scanner — scheduled + on-demand search for leaked credentials
//! in configured filesystem paths.
//!
//! Architecture mirrors `crate::metrics`:
//!   * the `secretshape` crate — what to look for (the rule table that used to
//!     be `rules.rs` here, now published standalone by the same author; rule
//!     names are byte-identical, so `secret_finding` dedupe hashes are stable)
//!   * `scanner` — the file walker + per-line matching (sync, runs in
//!     `spawn_blocking`)
//!   * `storage` — persistence and dashboard queries
//!   * this file — public API: `spawn_scheduler`, `run_scan`.
pub mod routes; // HTTP handlers for this admin feature (see admin/mod.rs)

pub mod scanner;
pub mod storage;

pub use scanner::Finding;
pub use storage::{FindingRow, LastScan, StatusCounts};

use crate::AppState;
use serde_json::json;
use std::time::Duration;
use tracing::{error, info};

/// Default scan cadence — six hours is enough to catch a fresh leak without
/// hammering the filesystem.
pub const SCAN_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// Spawns the periodic scanner. Does nothing if `secret_scan_paths` is empty.
pub fn spawn_scheduler(state: AppState) {
    if state.config().secret_scan_paths.is_empty() {
        info!("secret scanner: no paths configured, scheduler disabled");
        return;
    }

    tokio::spawn(async move {
        // Run once shortly after start-up, then every SCAN_INTERVAL.
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            if let Err(e) = run_scan(&state).await {
                error!(error = %e, "secret scan failed");
            }
            tokio::time::sleep(SCAN_INTERVAL).await;
        }
    });
}

/// Run a single scan synchronously (from the caller's perspective; the
/// actual filesystem walk runs on a blocking thread).  Updates the
/// `scan_run` history, persists new findings, and fires a Discord webhook
/// if any *new* critical findings were detected.
pub async fn run_scan(state: &AppState) -> anyhow::Result<()> {
    let roots = state.config().secret_scan_paths.clone();
    if roots.is_empty() {
        return Ok(());
    }

    info!(paths = ?roots, "starting secret scan");

    let (findings, counters) = tokio::task::spawn_blocking(move || scanner::scan(&roots)).await?;

    let total = findings.len();
    let critical = findings
        .iter()
        .filter(|f| matches!(f.severity, secretshape::Severity::Critical))
        .count();

    let new_count = storage::record_scan(state, findings, counters.files_scanned, counters.bytes_scanned, None).await?;

    info!(
        files = counters.files_scanned,
        total,
        critical,
        new = new_count,
        "secret scan finished"
    );

    // Alert on *new* critical findings only.
    if new_count > 0 && critical > 0 && state.has_any_alert_sink() {
        let payload = json!({
            "username": "klappstuhl secrets",
            "embeds": [{
                "title": "🔑 New secret leak detected",
                "description": format!(
                    "{new_count} new finding(s) in the latest scan, {critical} of which are CRITICAL severity.\n\nReview them at /admin/secrets."
                ),
                "color": 0xef4444,
            }]
        });
        state.send_alert(payload);
    }

    Ok(())
}
