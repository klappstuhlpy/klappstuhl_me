//! Audit log — every state-changing action lands in the `audit_log`
//! table with who/what/when/where/why.
//!
//! Callers build entries with a tiny `AuditBuilder` so handlers can stay
//! one-liners:
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
//! `fire()` is non-blocking — it spawns the insert as a fire-and-forget
//! task so audit logging never delays the HTTP response.  Failures are
//! logged to tracing at `warn` level; the request always succeeds.

use crate::{models::Account, AppState};
use serde::Serialize;
use serde_json::Value;
use std::net::IpAddr;
use time::OffsetDateTime;
use tracing::warn;

/// One audit log row as read back from the database (for the dashboard).
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: OffsetDateTime,
    pub actor_id: Option<i64>,
    pub actor_label: String,
    pub action: String,
    pub target: Option<String>,
    pub ip: Option<String>,
    /// Parsed back from the stored JSON string into a Value for templating.
    pub meta: Option<Value>,
}

impl AuditEntry {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let meta_str: Option<String> = row.get("meta_json")?;
        Ok(Self {
            id: row.get("id")?,
            ts: row.get("ts")?,
            actor_id: row.get("actor_id")?,
            actor_label: row.get("actor_label")?,
            action: row.get("action")?,
            target: row.get("target")?,
            ip: row.get("ip")?,
            meta: meta_str.and_then(|s| serde_json::from_str(&s).ok()),
        })
    }
}

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

    pub fn actor(mut self, account: &Account) -> Self {
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
        let state = self.state.clone();
        let action = self.action;
        let actor_id = self.actor_id;
        let actor_label = self.actor_label;
        let target = self.target;
        let ip = self.ip;
        let meta_json = self.meta.map(|v| v.to_string());

        tokio::spawn(async move {
            let result = state
                .database()
                .execute(
                    "INSERT INTO audit_log(actor_id, actor_label, action, target, ip, meta_json)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    (actor_id, actor_label.clone(), action, target.clone(), ip.clone(), meta_json),
                )
                .await;
            if let Err(e) = result {
                warn!(error = %e, action, actor = %actor_label, "failed to write audit log row");
            }
        });
    }
}

// ─── Dashboard reads ─────────────────────────────────────────────

#[derive(Debug, Serialize, Default)]
pub struct AuditCounts {
    pub today: i64,
    pub failed_logins_24h: i64,
    pub admin_actions_24h: i64,
    pub total: i64,
}

pub async fn counts(state: &AppState) -> rusqlite::Result<AuditCounts> {
    state
        .database()
        .call(|conn| -> rusqlite::Result<AuditCounts> {
            let mut c = AuditCounts::default();
            c.total = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))?;
            c.today = conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE ts >= date('now')",
                [],
                |r| r.get(0),
            )?;
            c.failed_logins_24h = conn.query_row(
                "SELECT COUNT(*) FROM audit_log
                 WHERE action = 'auth.login.fail' AND ts >= datetime('now', '-1 day')",
                [],
                |r| r.get(0),
            )?;
            c.admin_actions_24h = conn.query_row(
                "SELECT COUNT(*) FROM audit_log
                 WHERE (action LIKE 'service.%'
                     OR action LIKE 'invite.%'
                     OR action LIKE 'secret.%'
                     OR action LIKE 'admin.%')
                   AND ts >= datetime('now', '-1 day')",
                [],
                |r| r.get(0),
            )?;
            Ok(c)
        })
        .await
}

pub async fn list_recent(
    state: &AppState,
    action_prefix: Option<&str>,
    actor: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<AuditEntry>> {
    let action_filter = action_prefix.map(|s| format!("{s}%"));
    let actor_filter = actor.map(|s| s.to_string());

    state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<AuditEntry>> {
            // Two indexed-string filters; build the WHERE dynamically.
            let mut sql = "SELECT id, ts, actor_id, actor_label, action, target, ip, meta_json
                           FROM audit_log WHERE 1=1"
                .to_string();
            if action_filter.is_some() {
                sql.push_str(" AND action LIKE ?");
            }
            if actor_filter.is_some() {
                sql.push_str(" AND actor_label = ?");
            }
            sql.push_str(" ORDER BY id DESC LIMIT ?");

            let mut stmt = conn.prepare(&sql)?;
            let result: rusqlite::Result<Vec<_>> = match (action_filter, actor_filter) {
                (Some(a), Some(u)) => stmt
                    .query_map(rusqlite::params![a, u, limit], AuditEntry::from_row)?
                    .collect(),
                (Some(a), None) => stmt
                    .query_map(rusqlite::params![a, limit], AuditEntry::from_row)?
                    .collect(),
                (None, Some(u)) => stmt
                    .query_map(rusqlite::params![u, limit], AuditEntry::from_row)?
                    .collect(),
                (None, None) => stmt
                    .query_map(rusqlite::params![limit], AuditEntry::from_row)?
                    .collect(),
            };
            result
        })
        .await
}
