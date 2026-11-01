//! App-neutral audit-log writer/reader over a [`Database`] handle.
//!
//! This module knows the shape of the `audit_log` table and nothing else about
//! any consuming app. Each app points it at its own database and wraps it with
//! whatever app-specific side effects it needs — live-event publishing, an
//! `AppState::audit()` convenience builder, background-spawned writes. The
//! dotted action-name convention (`auth.login.fail`, `firewall.rule.*`,
//! `proxy.apply`, …) is owned by the caller; this module never interprets an
//! action string, so those names stay stable strings other code can read back.
//!
//! The `audit_log` table it expects:
//!
//! ```sql
//! CREATE TABLE audit_log (
//!     id          INTEGER PRIMARY KEY,
//!     ts          TEXT NOT NULL DEFAULT (datetime('now')),
//!     actor_id    INTEGER,      -- nullable (anonymous / system / deleted account)
//!     actor_label TEXT NOT NULL,
//!     action      TEXT NOT NULL,
//!     target      TEXT,
//!     ip          TEXT,
//!     meta_json   TEXT          -- serialised JSON object, or NULL
//! );
//! ```

use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use tracing::warn;

use crate::Database;

/// The columns for one audit row, built by the caller and persisted by
/// [`write`]. Kept a plain data struct so an app can construct it from its own
/// fluent builder (which is where app-specific concerns like live publishing
/// live).
#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub action: String,
    pub actor_id: Option<i64>,
    pub actor_label: String,
    pub target: Option<String>,
    pub ip: Option<String>,
    pub meta: Option<Value>,
}

impl AuditRecord {
    /// A record with just an action, an `anonymous` actor label, and no other
    /// fields — the caller fills the rest in.
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            actor_id: None,
            actor_label: "anonymous".to_string(),
            target: None,
            ip: None,
            meta: None,
        }
    }
}

/// Insert one audit row. Best-effort: a failure is logged at `warn` and
/// swallowed, because audit logging must never fail the request that triggered
/// it. Callers typically invoke this from a spawned task so the response isn't
/// delayed by the insert.
pub async fn write(db: &Database, record: &AuditRecord) {
    let meta_json = record.meta.as_ref().map(|v| v.to_string());
    let action = record.action.clone();
    let actor_label = record.actor_label.clone();
    let result = db
        .execute(
            "INSERT INTO audit_log(actor_id, actor_label, action, target, ip, meta_json)
             VALUES (?, ?, ?, ?, ?, ?)",
            (
                record.actor_id,
                actor_label.clone(),
                action.clone(),
                record.target.clone(),
                record.ip.clone(),
                meta_json,
            ),
        )
        .await;
    if let Err(e) = result {
        warn!(error = %e, action = %action, actor = %actor_label, "failed to write audit log row");
    }
}

/// One audit row as read back from the database (for a dashboard view).
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    // Always emitted as RFC3339 so JavaScript's `new Date(...)` can parse it.
    // Without the explicit `with`, the time crate's default Iso8601 output
    // includes a sub-second precision JS sometimes rejects.
    #[serde(with = "time::serde::rfc3339")]
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

/// Headline counts for the audit dashboard.
#[derive(Debug, Serialize, Default)]
pub struct AuditCounts {
    pub today: i64,
    pub failed_logins_24h: i64,
    pub admin_actions_24h: i64,
    pub total: i64,
}

/// Compute the headline audit counts. The `admin_actions_24h` bucket keys off
/// the stable dotted prefixes (`service.*`, `invite.*`, `secret.*`, `admin.*`)
/// — a coarse "how much happened" gauge, not an authorisation decision.
pub async fn counts(db: &Database) -> rusqlite::Result<AuditCounts> {
    db.call(|conn| -> rusqlite::Result<AuditCounts> {
        let total = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))?;
        let today = conn.query_row("SELECT COUNT(*) FROM audit_log WHERE ts >= date('now')", [], |r| {
            r.get(0)
        })?;
        let failed_logins_24h = conn.query_row(
            "SELECT COUNT(*) FROM audit_log
             WHERE action = 'auth.login.fail' AND ts >= datetime('now', '-1 day')",
            [],
            |r| r.get(0),
        )?;
        let admin_actions_24h = conn.query_row(
            "SELECT COUNT(*) FROM audit_log
             WHERE (action LIKE 'service.%'
                 OR action LIKE 'invite.%'
                 OR action LIKE 'secret.%'
                 OR action LIKE 'admin.%')
               AND ts >= datetime('now', '-1 day')",
            [],
            |r| r.get(0),
        )?;
        Ok(AuditCounts {
            today,
            failed_logins_24h,
            admin_actions_24h,
            total,
        })
    })
    .await
}

/// Read recent audit rows, newest first, optionally filtered by an action
/// prefix (`LIKE '<prefix>%'`) and/or an exact actor label.
pub async fn list_recent(
    db: &Database,
    action_prefix: Option<&str>,
    actor: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<AuditEntry>> {
    let action_filter = action_prefix.map(|s| format!("{s}%"));
    let actor_filter = actor.map(|s| s.to_string());

    db.call(move |conn| -> rusqlite::Result<Vec<AuditEntry>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    async fn memory_db() -> Database {
        let db = Database::file(":memory:")
            .connections(1)
            .open()
            .await
            .expect("open in-memory db");
        db.execute_batch(
            "CREATE TABLE audit_log (
                id          INTEGER PRIMARY KEY,
                ts          TEXT NOT NULL DEFAULT (datetime('now')),
                actor_id    INTEGER,
                actor_label TEXT NOT NULL,
                action      TEXT NOT NULL,
                target      TEXT,
                ip          TEXT,
                meta_json   TEXT
            );",
        )
        .await
        .expect("create audit_log");
        db
    }

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let db = memory_db().await;
        let mut rec = AuditRecord::new("auth.login.fail");
        rec.actor_label = "someone".into();
        rec.ip = Some("203.0.113.7".into());
        rec.meta = Some(serde_json::json!({ "reason": "bad password" }));
        write(&db, &rec).await;

        let rows = list_recent(&db, None, None, 10).await.expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "auth.login.fail");
        assert_eq!(rows[0].actor_label, "someone");
        assert_eq!(rows[0].ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(rows[0].meta.as_ref().unwrap()["reason"], "bad password");
    }

    #[tokio::test]
    async fn filters_and_counts() {
        let db = memory_db().await;
        for _ in 0..3 {
            write(&db, &AuditRecord::new("auth.login.fail")).await;
        }
        let mut invite = AuditRecord::new("invite.create");
        invite.actor_label = "admin".into();
        write(&db, &invite).await;

        let fails = list_recent(&db, Some("auth.login."), None, 10).await.unwrap();
        assert_eq!(fails.len(), 3);
        let by_actor = list_recent(&db, None, Some("admin"), 10).await.unwrap();
        assert_eq!(by_actor.len(), 1);
        assert_eq!(by_actor[0].action, "invite.create");

        let c = counts(&db).await.unwrap();
        assert_eq!(c.total, 4);
        assert_eq!(c.failed_logins_24h, 3);
        assert_eq!(c.admin_actions_24h, 1); // invite.* counts, auth.* doesn't
    }
}
