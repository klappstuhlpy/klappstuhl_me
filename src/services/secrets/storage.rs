//! Persistence layer for the secret scanner.
//!
//! `upsert_findings` keeps the table from growing on every scan: if a
//! finding already exists (same hash), only `last_seen` is bumped.  The
//! function returns how many findings were *new* in the current run so the
//! orchestration code can decide whether to fire a webhook.

use crate::AppState;
use serde::Serialize;
use time::OffsetDateTime;

use super::scanner::Finding;

/// Persist a scan_run row and every finding emitted by it.
///
/// Returns the number of findings that were brand new (i.e. didn't already
/// exist in `secret_finding`).  Used to decide whether to fire the alert
/// webhook.
pub async fn record_scan(
    state: &AppState,
    findings: Vec<Finding>,
    files_scanned: u64,
    bytes_scanned: u64,
    error: Option<String>,
) -> rusqlite::Result<i64> {
    state
        .database()
        .call(move |conn| -> rusqlite::Result<i64> {
            let tx = conn.transaction()?;

            // Insert the scan_run header up-front so we can count "new" by
            // comparing rowcounts before / after.
            tx.execute(
                "INSERT INTO scan_run(files_scanned, bytes_scanned, error)
                 VALUES (?, ?, ?)",
                rusqlite::params![files_scanned as i64, bytes_scanned as i64, error],
            )?;
            let run_id: i64 = tx.last_insert_rowid();

            let total = findings.len() as i64;
            let mut new_count: i64 = 0;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO secret_finding
                        (rule, severity, file_path, line, snippet, finding_hash, status,
                         first_seen, last_seen)
                     VALUES (?, ?, ?, ?, ?, ?, 'open', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                     ON CONFLICT(finding_hash) DO UPDATE SET last_seen = CURRENT_TIMESTAMP",
                )?;
                for f in findings {
                    let inserted = stmt.execute(rusqlite::params![
                        f.rule,
                        f.severity.as_str(),
                        f.file_path.to_string_lossy().into_owned(),
                        f.line as i64,
                        f.snippet,
                        f.finding_hash,
                    ])?;
                    // SQLite's INSERT … ON CONFLICT returns 1 for both
                    // insert and the update branch — we can't tell which
                    // happened from the rowcount alone.  Resolve with
                    // changes() reset trick.
                    if inserted == 1 {
                        // Determine if it was a new row by checking if the
                        // first_seen == last_seen for this finding.  In
                        // SQLite both default to CURRENT_TIMESTAMP, so we
                        // compare them in the same statement.
                        let was_new: bool = tx
                            .prepare_cached(
                                "SELECT first_seen = last_seen
                                 FROM secret_finding
                                 WHERE finding_hash = ?",
                            )?
                            .query_row([&f.finding_hash], |r| r.get(0))?;
                        if was_new {
                            new_count += 1;
                        }
                    }
                }
            }

            tx.execute(
                "UPDATE scan_run
                 SET finished_at    = CURRENT_TIMESTAMP,
                     findings_new   = ?,
                     findings_total = ?
                 WHERE id = ?",
                rusqlite::params![new_count, total, run_id],
            )?;
            tx.commit()?;
            Ok(new_count)
        })
        .await
}

#[derive(Debug, Serialize)]
pub struct FindingRow {
    pub id: i64,
    pub rule: String,
    pub severity: String,
    pub file_path: String,
    pub line: i64,
    pub snippet: String,
    pub status: String,
    // Force RFC 3339 so JS `new Date(...)` can parse it. Without `with`,
    // the time crate's default serde format is space-separated and JS
    // returns NaN, so the dashboard can't compute "Xm ago".
    #[serde(with = "time::serde::rfc3339")]
    pub first_seen: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
}

/// `status_filter`: `Some("open"|"dismissed"|"resolved")` filters to that
/// status; `None` returns everything.
pub async fn list_findings(
    state: &AppState,
    status_filter: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<FindingRow>> {
    let status = status_filter.map(|s| s.to_string());
    state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<FindingRow>> {
            let sql = if status.is_some() {
                "SELECT id, rule, severity, file_path, line, snippet, status,
                        first_seen, last_seen
                 FROM secret_finding
                 WHERE status = ?
                 ORDER BY
                     CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 ELSE 2 END,
                     last_seen DESC
                 LIMIT ?"
            } else {
                "SELECT id, rule, severity, file_path, line, snippet, status,
                        first_seen, last_seen
                 FROM secret_finding
                 ORDER BY
                     CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 ELSE 2 END,
                     last_seen DESC
                 LIMIT ?"
            };
            let mut stmt = conn.prepare_cached(sql)?;
            let row_map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<FindingRow> {
                Ok(FindingRow {
                    id: row.get(0)?,
                    rule: row.get(1)?,
                    severity: row.get(2)?,
                    file_path: row.get(3)?,
                    line: row.get(4)?,
                    snippet: row.get(5)?,
                    status: row.get(6)?,
                    first_seen: row.get(7)?,
                    last_seen: row.get(8)?,
                })
            };
            let rows = if let Some(s) = status {
                stmt.query_map(rusqlite::params![s, limit], row_map)?.collect()
            } else {
                stmt.query_map(rusqlite::params![limit], row_map)?.collect()
            };
            rows
        })
        .await
}

#[derive(Debug, Serialize, Default)]
pub struct StatusCounts {
    pub open: i64,
    pub critical_open: i64,
    pub dismissed: i64,
    pub resolved: i64,
}

pub async fn status_counts(state: &AppState) -> rusqlite::Result<StatusCounts> {
    state
        .database()
        .call(|conn| -> rusqlite::Result<StatusCounts> {
            let mut s = StatusCounts::default();
            s.open = conn.query_row("SELECT COUNT(*) FROM secret_finding WHERE status = 'open'", [], |r| {
                r.get(0)
            })?;
            s.critical_open = conn.query_row(
                "SELECT COUNT(*) FROM secret_finding
                 WHERE status = 'open' AND severity = 'critical'",
                [],
                |r| r.get(0),
            )?;
            s.dismissed = conn.query_row(
                "SELECT COUNT(*) FROM secret_finding WHERE status = 'dismissed'",
                [],
                |r| r.get(0),
            )?;
            s.resolved = conn.query_row(
                "SELECT COUNT(*) FROM secret_finding WHERE status = 'resolved'",
                [],
                |r| r.get(0),
            )?;
            Ok(s)
        })
        .await
}

#[derive(Debug, Serialize, Default)]
pub struct LastScan {
    // See FindingRow above — RFC 3339 needed for JS `new Date()`.
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub files_scanned: i64,
    pub findings_new: i64,
    pub findings_total: i64,
    pub error: Option<String>,
}

pub async fn last_scan(state: &AppState) -> rusqlite::Result<Option<LastScan>> {
    state
        .database()
        .call(|conn| -> rusqlite::Result<Option<LastScan>> {
            let mut stmt = conn.prepare_cached(
                "SELECT started_at, finished_at, files_scanned, findings_new,
                        findings_total, error
                 FROM scan_run
                 ORDER BY id DESC
                 LIMIT 1",
            )?;
            let result = stmt.query_row([], |row| {
                Ok(LastScan {
                    started_at: row.get(0)?,
                    finished_at: row.get(1)?,
                    files_scanned: row.get(2)?,
                    findings_new: row.get(3)?,
                    findings_total: row.get(4)?,
                    error: row.get(5)?,
                })
            });
            match result {
                Ok(v) => Ok(Some(v)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e),
            }
        })
        .await
}

pub async fn set_status(state: &AppState, id: i64, status: &str) -> rusqlite::Result<usize> {
    let status = status.to_string();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<usize> {
            conn.execute(
                "UPDATE secret_finding SET status = ? WHERE id = ?",
                rusqlite::params![status, id],
            )
        })
        .await
}
