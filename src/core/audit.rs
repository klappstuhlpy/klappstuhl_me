//! Audit log — every state-changing action lands in the `audit_log` table with
//! who/what/when/where/why.
//!
//! The generic writer/reader (the `audit_log` DDL knowledge, the DB insert, and
//! the dashboard read queries) lives in **`kls-web-core::audit`**, shared with
//! the standalone apps that split out of this monolith. This module is the thin
//! app-side shim: it re-exports the shared read models and owns the two
//! app-specific concerns the kernel can't — the fluent [`AuditBuilder`] hung off
//! `AppState`, and pushing each entry to the admin `/ws` live feed as it's
//! written.
//!
//! Callers build entries with the builder so handlers stay one-liners:
//!
//! ```ignore
//! state.audit("invite.create")
//!      .actor(&account)
//!      .target(&code)
//!      .ip_opt(client_ip)
//!      .meta(serde_json::json!({ "expires_in_days": form.expires_in_days }))
//!      .fire();
//! ```
//!
//! `fire()` is non-blocking — it spawns the insert as a fire-and-forget task so
//! audit logging never delays the HTTP response. Failures are logged by the
//! shared writer at `warn`; the request always succeeds.

use crate::AppState;
use serde_json::Value;
use std::net::IpAddr;

// Re-export the shared read models so `crate::audit::{AuditEntry, AuditCounts}`
// (and `AuditRecord`, the wire the builder produces) keep resolving unchanged.
pub use kls_web_core::audit::{AuditCounts, AuditEntry, AuditRecord};

/// Fluent builder. Cheap to construct, finalised by `.fire()`.
pub struct AuditBuilder<'a> {
    state: &'a AppState,
    action: &'static str,
    actor_id: Option<i64>,
    actor_label: String,
    target: Option<String>,
    ip: Option<String>,
    meta: Option<Value>,
}

impl<'a> AuditBuilder<'a> {
    pub fn new(state: &'a AppState, action: &'static str) -> Self {
        Self {
            state,
            action,
            actor_id: None,
            actor_label: "anonymous".to_string(),
            target: None,
            ip: None,
            meta: None,
        }
    }

    pub fn actor(mut self, account: &crate::models::Account) -> Self {
        self.actor_id = Some(account.id);
        self.actor_label = account.name.clone();
        self
    }

    /// Used when the actor is identified only by username (e.g. failed
    /// login — no account row exists yet but we still want a label).
    pub fn actor_label(mut self, label: impl Into<String>) -> Self {
        self.actor_label = label.into();
        self
    }

    pub fn target(mut self, t: impl Into<String>) -> Self {
        self.target = Some(t.into());
        self
    }

    pub fn ip(mut self, ip: IpAddr) -> Self {
        self.ip = Some(ip.to_string());
        self
    }

    pub fn ip_opt(mut self, ip: Option<IpAddr>) -> Self {
        self.ip = ip.map(|i| i.to_string());
        self
    }

    pub fn meta(mut self, value: Value) -> Self {
        self.meta = Some(value);
        self
    }

    /// Spawn the insert in the background. The caller's response is never delayed.
    pub fn fire(self) {
        let record = AuditRecord {
            action: self.action.to_string(),
            actor_id: self.actor_id,
            actor_label: self.actor_label,
            target: self.target,
            ip: self.ip,
            meta: self.meta,
        };

        let state = self.state.clone();
        tokio::spawn(async move {
            kls_web_core::audit::write(state.database(), &record).await;
        });
    }
}

// ─── Dashboard reads (delegate to the shared kernel over this app's DB) ──────

pub async fn counts(state: &AppState) -> rusqlite::Result<AuditCounts> {
    kls_web_core::audit::counts(state.database()).await
}

pub async fn list_recent(
    state: &AppState,
    action_prefix: Option<&str>,
    actor: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<AuditEntry>> {
    kls_web_core::audit::list_recent(state.database(), action_prefix, actor, limit).await
}
