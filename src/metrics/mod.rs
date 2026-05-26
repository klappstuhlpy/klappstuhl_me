//! Live server metrics — collection, storage, and alerts.
//!
//! ## Overview
//!
//! Three concerns live in this module:
//!
//! 1. **Collection** (`host`, `docker`): scrape one snapshot of the system.
//!    The host module parses `/proc` and `/sys` directly (paths overridable via
//!    `HOST_PROC` / `HOST_SYS` env vars so the app can read host metrics from
//!    inside a container).  The docker module shells out to `docker stats`.
//!
//! 2. **Storage** (`storage`): write samples into the `metric_sample` and
//!    `docker_stat` tables.  See `sql/3.sql` for the schema.
//!
//! 3. **Alerts** (`alerts`): hard-coded thresholds (CPU > 90% for 5 min,
//!    RAM > 90%, disk > 90%, temp > 80 °C) fire a Discord webhook on the
//!    OK → ALERT transition with a 30-minute cooldown.
//!
//! A single background task (`spawn_collector`) ties these together, scraping
//! every 30 seconds and running the alert check after each insert.  A second
//! task (`spawn_pruner`) trims samples older than 30 days hourly.

mod alerts;
pub mod docker;
mod host;
pub mod storage;

pub use alerts::AlertState;
pub use docker::DockerStat;
pub use host::Sample;
pub use storage::{
    fetch_current, fetch_docker_history, fetch_history, CurrentView, DockerHistoryPoint,
    HistoryPoint,
};

use crate::AppState;
use std::time::Duration;
use tracing::{error, info};

/// How often the collector scrapes /proc, /sys and `docker stats`.
pub const SCRAPE_INTERVAL: Duration = Duration::from_secs(30);

/// How long samples are kept before pruning.
pub const RETENTION: time::Duration = time::Duration::days(30);

/// Spawns the background scrape task. Runs forever; logs and continues on error.
pub fn spawn_collector(state: AppState) {
    let alert_state = AlertState::default();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SCRAPE_INTERVAL);
        // Skip the immediate-fire on the first tick — we want a sane delay
        // after startup before the first scrape.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await; // consume the immediate tick

        loop {
            interval.tick().await;
            match scrape_once(&state, &alert_state).await {
                Ok(()) => {}
                Err(e) => error!(error = %e, "metrics scrape failed"),
            }
        }
    });
}

/// Spawns the hourly pruner that drops samples older than [`RETENTION`].
pub fn spawn_pruner(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        interval.tick().await; // consume immediate
        loop {
            interval.tick().await;
            if let Err(e) = storage::prune_older_than(&state, RETENTION).await {
                error!(error = %e, "metric prune failed");
            }
        }
    });
}

async fn scrape_once(state: &AppState, alert_state: &AlertState) -> anyhow::Result<()> {
    // Heartbeat so every scrape attempt is visible in the log file,
    // even when something downstream hangs or no errors fire.
    info!(target: "metrics", "scrape: starting");

    let sample = host::collect().await.map_err(|e| {
        tracing::error!(target: "metrics", error = %e, "host::collect failed");
        e
    })?;
    let containers = match docker::collect().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "metrics", error = %e, "docker::collect failed (continuing)");
            Vec::new()
        }
    };
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();

    storage::insert_sample(state, ts, &sample).await.map_err(|e| {
        tracing::error!(target: "metrics", error = %e, "insert_sample failed — check that the four disk_* columns exist on metric_sample (PRAGMA table_info(metric_sample))");
        e
    })?;
    storage::insert_docker_stats(state, ts, &containers).await.map_err(|e| {
        tracing::error!(target: "metrics", error = %e, "insert_docker_stats failed");
        e
    })?;

    alerts::check_and_fire(state, alert_state, &sample).await;

    // Push to /ws subscribers so live dashboards refresh without polling.
    // We ship a compact subset of fields — enough for the tile row +
    // container table. Charts still use the /history endpoint.
    state.live_publish(
        "metrics",
        serde_json::json!({
            "ts": ts,
            "cpu_total": sample.cpu_total_pct(),
            "mem_used": sample.mem_used as i64,
            "mem_total": sample.mem_total as i64,
            "mem_used_pct": sample.mem_used_pct(),
            "disk_used": sample.disk_used as i64,
            "disk_total": sample.disk_total as i64,
            "disk_used_pct": sample.disk_used_pct(),
            "load_1": sample.load_1,
            "load_5": sample.load_5,
            "load_15": sample.load_15,
            "net_rx_bytes": sample.net_rx_bytes as i64,
            "net_tx_bytes": sample.net_tx_bytes as i64,
            "disk_read_bytes":  sample.disk_read_bytes as i64,
            "disk_write_bytes": sample.disk_write_bytes as i64,
            "disk_read_ops":  sample.disk_read_ops as i64,
            "disk_write_ops": sample.disk_write_ops as i64,
            "containers": containers,
        }),
    );

    info!(target: "metrics", "scrape ok: cpu={:.1}% mem={:.1}% containers={}",
          sample.cpu_total_pct(),
          sample.mem_used_pct(),
          containers.len());
    Ok(())
}
