//! Firewall management — visual frontend for `nftables`, `ufw`, `iptables`.
//!
//! Approach: the backend is detected at startup by probing each binary in
//! turn. The first one that responds wins; admins can override via
//! `firewall.backend` in `config.json`.
//!
//! Rules are stored in `firewall_rule` (our mirror) and applied by
//! shelling out to the matching backend. Audit logging happens at the
//! HTTP route layer.

pub mod backend;
pub mod lockout;
pub mod storage;

pub use backend::{Backend, BackendKind};
pub use storage::{FirewallRule, LockoutRow, NewRule};

use crate::AppState;
use std::time::Duration;
use tracing::{error, info};

/// Hooks the firewall background tasks:
///   * lockout reaper — releases expired auto-blocks every minute.
pub fn spawn_workers(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            match lockout::reap_expired(&state).await {
                Ok(n) if n > 0 => info!(count = n, "firewall: released {n} expired lockouts"),
                Ok(_) => {}
                Err(e) => error!(error = %e, "firewall: lockout reaper failed"),
            }
        }
    });
}
