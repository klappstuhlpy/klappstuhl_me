//! Threshold alerts.
//!
//! Fires a Discord webhook on the OK → ALERT state transition. A 30-minute
//! cooldown stops repeated notifications for the same condition. CPU also has
//! a sustained-window requirement (5 minutes above threshold before firing)
//! to avoid spamming on transient spikes.

use crate::AppState;
use serde_json::json;
use std::sync::Mutex;
use time::OffsetDateTime;

use super::Sample;

const CPU_THRESHOLD: f64 = 90.0; // percent
const CPU_SUSTAIN_SECS: i64 = 5 * 60; // must be above for this long
const MEM_THRESHOLD: f64 = 90.0; // percent
const DISK_THRESHOLD: f64 = 90.0; // percent
const COOLDOWN_SECS: i64 = 30 * 60; // suppress repeat alerts for 30 min

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    /// Threshold is currently exceeded. `since` is the first second it crossed.
    Pending(i64),
    /// We already fired an alert at this timestamp.
    Firing(i64),
}

impl Default for Status {
    fn default() -> Self {
        Self::Ok
    }
}

#[derive(Default)]
struct Tracker {
    cpu: Status,
    mem: Status,
    disk: Status,
}

/// Shared alert state held by the collector task. Wrapped in a Mutex because
/// alert evaluation borrows it mutably while still inside the async task.
#[derive(Default, Clone)]
pub struct AlertState {
    inner: std::sync::Arc<Mutex<Tracker>>,
}

/// Evaluates each metric against its threshold and fires webhooks for any
/// OK → ALERT transitions.  Should be called once per scrape.
pub async fn check_and_fire(state: &AppState, alerts: &AlertState, sample: &Sample) {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut fires: Vec<(String, String)> = Vec::new();

    {
        let mut t = alerts.inner.lock().unwrap();

        // CPU — requires sustained crossing
        let cpu = sample.cpu_total_pct();
        match check_sustained(&mut t.cpu, cpu, CPU_THRESHOLD, CPU_SUSTAIN_SECS, now) {
            CheckOutcome::Fired => fires.push((
                "🔥 High CPU".into(),
                format!(
                    "CPU at {cpu:.1}% (≥ {CPU_THRESHOLD}%) for over {} min",
                    CPU_SUSTAIN_SECS / 60
                ),
            )),
            _ => {}
        }

        // Memory — instant
        let mem = sample.mem_used_pct();
        if let CheckOutcome::Fired = check_instant(&mut t.mem, mem, MEM_THRESHOLD, now) {
            fires.push((
                "🧠 High memory".into(),
                format!("Memory at {mem:.1}% (≥ {MEM_THRESHOLD}%)"),
            ));
        }

        // Disk — instant
        let disk = sample.disk_used_pct();
        if let CheckOutcome::Fired = check_instant(&mut t.disk, disk, DISK_THRESHOLD, now) {
            fires.push((
                "💾 Disk almost full".into(),
                format!("Root filesystem at {disk:.1}% (≥ {DISK_THRESHOLD}%)"),
            ));
        }
    } // drop the mutex guard before awaiting

    if fires.is_empty() {
        return;
    }
    if !state.has_any_alert_sink() {
        return; // nowhere to send
    }

    for (title, body) in fires {
        let payload = json!({
            "username": "klappstuhl monitor",
            "embeds": [{
                "title": title,
                "description": body,
                "color": 0xef4444,  // red
            }]
        });
        state.send_alert(payload);
    }
}

enum CheckOutcome {
    BelowThreshold,
    InCooldown,
    StillPending,
    Fired,
}

/// Instant alert: fires immediately on the first sample above threshold,
/// respects the cooldown.
fn check_instant(slot: &mut Status, value: f64, threshold: f64, now: i64) -> CheckOutcome {
    if value < threshold {
        *slot = Status::Ok;
        return CheckOutcome::BelowThreshold;
    }
    match *slot {
        Status::Firing(when) if now - when < COOLDOWN_SECS => CheckOutcome::InCooldown,
        _ => {
            *slot = Status::Firing(now);
            CheckOutcome::Fired
        }
    }
}

/// Sustained alert: only fires after the value has been above threshold for
/// `sustain_secs` continuously.
fn check_sustained(slot: &mut Status, value: f64, threshold: f64, sustain_secs: i64, now: i64) -> CheckOutcome {
    if value < threshold {
        *slot = Status::Ok;
        return CheckOutcome::BelowThreshold;
    }
    match *slot {
        Status::Ok => {
            *slot = Status::Pending(now);
            CheckOutcome::StillPending
        }
        Status::Pending(started) if now - started >= sustain_secs => {
            *slot = Status::Firing(now);
            CheckOutcome::Fired
        }
        Status::Pending(_) => CheckOutcome::StillPending,
        Status::Firing(when) if now - when < COOLDOWN_SECS => CheckOutcome::InCooldown,
        Status::Firing(_) => {
            *slot = Status::Firing(now);
            CheckOutcome::Fired
        }
    }
}
