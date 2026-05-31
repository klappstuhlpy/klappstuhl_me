//! Health checks / uptime monitoring — an internal Uptime-Kuma.
//!
//! Architecture:
//!   * `checker` — runs one probe (http / tcp / keyword / ssl), returns a
//!     [`CheckOutcome`].  Sync-free, async-only.
//!   * `storage` — persistence: targets CRUD, samples ring buffer pruning,
//!     incident open/close, aggregations for the dashboard.
//!   * this file — orchestration: `spawn_monitor` schedules per-target
//!     probe loops; `run_check_now` performs a one-off probe and records
//!     the result.

pub mod checker;
pub mod storage;

pub use checker::{CheckKind, CheckOutcome, CheckStatus};
pub use storage::{HealthTarget, IncidentRow, SampleRow, TargetSummary, UptimeStats};

use crate::AppState;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info};

/// How often the scheduler re-reads the target list from the DB.  New
/// targets added through the UI become live within this window.
const RELOAD_INTERVAL: Duration = Duration::from_secs(30);

/// How many consecutive failing samples trigger a new incident.  Set to 1
/// so users see incidents immediately — Uptime Kuma uses the same default.
const INCIDENT_THRESHOLD: i64 = 1;

/// Prune samples older than this on each loop tick.  30 days is plenty
/// for uptime stats and incident summaries.
pub const SAMPLE_RETENTION: Duration = Duration::from_secs(60 * 60 * 24 * 30);

/// Per-target task handle.  The scheduler reconciles this map against the
/// DB on every reload so adding/disabling/deleting a target in the UI
/// stops/starts the corresponding probe loop without a restart.
type TaskMap = Arc<Mutex<HashMap<i64, tokio::task::JoinHandle<()>>>>;

pub fn spawn_monitor(state: AppState) {
    let tasks: TaskMap = Arc::new(Mutex::new(HashMap::new()));

    // Background pruner — clean old samples once an hour.
    let prune_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            interval.tick().await;
            if let Err(e) = storage::prune_old_samples(&prune_state, SAMPLE_RETENTION).await {
                error!(error = %e, "health: sample pruning failed");
            }
        }
    });

    // Reconciler — keeps the per-target probe loops in sync with the DB.
    tokio::spawn(async move {
        // Initial small delay so the DB pool is warm.
        tokio::time::sleep(Duration::from_secs(2)).await;
        loop {
            if let Err(e) = reconcile(&state, &tasks).await {
                error!(error = %e, "health: reconcile failed");
            }
            tokio::time::sleep(RELOAD_INTERVAL).await;
        }
    });
}

async fn reconcile(state: &AppState, tasks: &TaskMap) -> anyhow::Result<()> {
    let targets = storage::list_targets(state).await?;
    let active_ids: std::collections::HashSet<i64> = targets.iter().filter(|t| t.enabled).map(|t| t.id).collect();

    let mut tasks_guard = tasks.lock().await;

    // Stop tasks whose target was deleted or disabled.
    let stale: Vec<i64> = tasks_guard
        .keys()
        .copied()
        .filter(|id| !active_ids.contains(id))
        .collect();
    for id in stale {
        if let Some(handle) = tasks_guard.remove(&id) {
            handle.abort();
            info!(target_id = id, "health: probe loop stopped");
        }
    }

    // Start tasks for newly added/enabled targets.
    for target in targets {
        if !target.enabled {
            continue;
        }
        if tasks_guard.contains_key(&target.id) {
            continue;
        }
        let bg_state = state.clone();
        let target_id = target.id;
        let interval_secs = target.interval_seconds.max(10) as u64;
        let handle = tokio::spawn(async move {
            // Stagger the very first probe so we don't slam a backend if
            // many targets have the same interval.
            tokio::time::sleep(Duration::from_secs((target_id as u64) % 7)).await;
            loop {
                if let Err(e) = run_check_now(&bg_state, target_id).await {
                    error!(target_id, error = %e, "health: probe failed");
                }
                tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            }
        });
        tasks_guard.insert(target.id, handle);
        info!(target_id = target.id, name = %target.name, "health: probe loop started");
    }
    Ok(())
}

/// Run one probe for a target and persist the result.  Public so the
/// "Check now" button can invoke it directly.  Returns the outcome so
/// the route can echo it back to the client.
pub async fn run_check_now(state: &AppState, target_id: i64) -> anyhow::Result<CheckOutcome> {
    let target = storage::get_target(state, target_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("target not found"))?;

    let outcome = checker::run(&target, &state.client).await;

    storage::record_sample(state, target_id, &outcome).await?;
    let prev_open = storage::get_open_incident(state, target_id).await?;

    match (&outcome.status, prev_open) {
        (CheckStatus::Up, Some(inc)) => {
            // Recovered → close the incident.
            storage::close_incident(state, inc.id).await?;
            broadcast_event(state, target_id, "recovered", &outcome);
            fire_recovery_webhook(state, &target);
        }
        (CheckStatus::Down | CheckStatus::Degraded, Some(inc)) => {
            // Extend the open incident.
            storage::extend_incident(state, inc.id, outcome.error.as_deref()).await?;
        }
        (CheckStatus::Down | CheckStatus::Degraded, None) => {
            // Open a new incident once we hit the threshold.
            let consecutive = storage::consecutive_failures(state, target_id, INCIDENT_THRESHOLD).await?;
            if consecutive >= INCIDENT_THRESHOLD {
                let status_label = match outcome.status {
                    CheckStatus::Down => "down",
                    CheckStatus::Degraded => "degraded",
                    CheckStatus::Up => "up",
                };
                storage::open_incident(state, target_id, status_label, outcome.error.as_deref()).await?;
                broadcast_event(state, target_id, "down", &outcome);
                fire_down_webhook(state, &target, &outcome);
            }
        }
        (CheckStatus::Up, None) => { /* steady state */ }
    }

    state.live_publish(
        "health",
        json!({
            "target_id": target_id,
            "status": outcome.status_str(),
            "latency_ms": outcome.latency_ms,
            "status_code": outcome.status_code,
            "ssl_days_left": outcome.ssl_days_left,
            "error": outcome.error,
        }),
    );

    Ok(outcome)
}

fn broadcast_event(state: &AppState, target_id: i64, event: &'static str, outcome: &CheckOutcome) {
    state.live_publish(
        "health.event",
        json!({
            "target_id": target_id,
            "event": event,
            "status": outcome.status_str(),
            "latency_ms": outcome.latency_ms,
            "error": outcome.error,
        }),
    );
}

fn fire_down_webhook(state: &AppState, target: &HealthTarget, outcome: &CheckOutcome) {
    if !state.has_any_alert_sink() {
        return;
    }
    let dashboard_url = state.config().url_to("/admin/health");
    let payload = json!({
        "username": "klappstuhl health",
        "embeds": [{
            "title": format!("🔴 {} is {}", target.name, outcome.status_str()),
            "description": format!(
                "**Target:** `{}`\n**Kind:** {}\n**Error:** {}\n\nCheck the [/admin/health]({}) dashboard.",
                target.target,
                target.kind,
                outcome.error.as_deref().unwrap_or("unknown"),
                dashboard_url,
            ),
            "color": 0xef4444,
        }]
    });
    state.send_alert(payload);
}

fn fire_recovery_webhook(state: &AppState, target: &HealthTarget) {
    if !state.has_any_alert_sink() {
        return;
    }
    let payload = json!({
        "username": "klappstuhl health",
        "embeds": [{
            "title": format!("🟢 {} recovered", target.name),
            "description": format!("`{}` is back up.", target.target),
            "color": 0x10b981,
        }]
    });
    state.send_alert(payload);
}
