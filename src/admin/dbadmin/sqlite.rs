//! Internal SQLite backend for the database admin page.
//!
//! Browses the application's own on-disk databases — `main.db` (accounts,
//! sessions, images, audit log, …) and `requests.db` (the HTTP request
//! log). Rather than borrow a connection from the live pools (which would
//! contend with the running app and, for the request log, isn't exposed),
//! we open a fresh short-lived connection to the database file per request —
//! the same per-request philosophy the Postgres backend uses.
//!
//! Safe-mode sets `PRAGMA query_only = ON` on the connection, which makes
//! the SQLite engine reject every write (INSERT/UPDATE/DELETE/DDL) for the
//! lifetime of that connection — the bulletproof counterpart to Postgres'
//! read-only transaction.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{types::ValueRef, Connection};

use super::{DatabaseInfo, QueryResult, TableInfo, ROW_LIMIT};
use crate::AppState;

/// Which internal database to target.
#[derive(Debug, Clone, Copy)]
pub enum Which {
    Main,
    Requests,
}

impl Which {
    /// Parses the name portion of a `sqlite:<name>` source id.
    pub fn parse(name: &str) -> anyhow::Result<Self> {
        match name {
            "main" => Ok(Self::Main),
            "requests" => Ok(Self::Requests),
            other => anyhow::bail!("unknown internal database: {other}"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Requests => "requests",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Main => "main.db",
            Self::Requests => "requests.db",
        }
    }

    fn source_id(self) -> String {
        format!("sqlite:{}", self.name())
    }
}

/// Resolves the on-disk path for an internal database. Both live in the
/// app's data directory next to each other (see `core::database::directory`
/// and `core::logging::RequestLogger::new`).
fn db_path(which: Which) -> anyhow::Result<PathBuf> {
    let mut path = crate::database::directory()?; // <data>/main.db
    path.set_file_name(which.file_name());
    Ok(path)
}

/// Opens a fresh connection to a database file. When `read_only` is set,
/// `query_only` is engaged so the engine rejects any write on this
/// connection — the connection is short-lived and dropped after the request,
/// so there's nothing to reset.
fn open(path: &Path, read_only: bool) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    if read_only {
        conn.execute_batch("PRAGMA query_only = ON;")?;
    }
    Ok(conn)
}

// ─── Catalog reads ───────────────────────────────────────────────────

pub async fn list_databases(_state: &AppState) -> anyhow::Result<Vec<DatabaseInfo>> {
    let mut out = Vec::new();
    for which in [Which::Main, Which::Requests] {
        let path = db_path(which)?;
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        out.push(DatabaseInfo {
            id: which.source_id(),
            name: which.name().to_string(),
            kind: "sqlite",
            owner: "—".into(),
            encoding: "UTF-8".into(),
            size_pretty: crate::backup::human_size(size),
        });
    }
    Ok(out)
}

pub async fn list_tables(_state: &AppState, which: Which) -> anyhow::Result<Vec<TableInfo>> {
    let path = db_path(which)?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<TableInfo>> {
        let conn = open(&path, true)?;
        let names: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let mut out = Vec::with_capacity(names.len());
        for name in names {
            // Internal tables are small enough that an exact COUNT(*) is
            // cheap. The identifier is double-quoted (and inner quotes
            // doubled) so unusual table names can't break out of the quoting.
            let quoted = name.replace('"', "\"\"");
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), [], |r| r.get(0))
                .unwrap_or(0);
            out.push(TableInfo {
                schema: "main".into(),
                name,
                owner: "—".into(),
                row_estimate: count,
                size_pretty: "—".into(),
            });
        }
        Ok(out)
    })
    .await?
}

// ─── Query runner ────────────────────────────────────────────────────

pub async fn run_query(_state: &AppState, which: Which, sql: &str, safe: bool) -> anyhow::Result<QueryResult> {
    let path = db_path(which)?;
    let sql = sql.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<QueryResult> {
        let conn = open(&path, safe)?;
        let started = Instant::now();

        let mut stmt = conn.prepare(&sql)?;
        let col_count = stmt.column_count();

        // A statement with no result columns (INSERT/UPDATE/DELETE/DDL/…)
        // must be run with `execute`, not `query`. In safe mode `query_only`
        // makes the engine reject it before any rows are touched.
        if col_count == 0 {
            let affected = stmt.execute([])?;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: affected,
                elapsed_ms,
                truncated: false,
            });
        }

        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

        let mut rows_iter = stmt.query([])?;
        let mut cells: Vec<Vec<String>> = Vec::new();
        let mut total = 0usize;
        while let Some(row) = rows_iter.next()? {
            total += 1;
            if cells.len() < ROW_LIMIT {
                cells.push((0..col_count).map(|i| value_to_string(row, i)).collect());
            }
        }
        let elapsed_ms = started.elapsed().as_millis() as u64;

        Ok(QueryResult {
            columns,
            rows: cells,
            row_count: total,
            elapsed_ms,
            truncated: total > ROW_LIMIT,
        })
    })
    .await?
}

/// Coerces one SQLite cell to a display string. Blobs are summarised by
/// length rather than dumped, so a `SELECT *` over a table with binary
/// columns (e.g. `images.image_data`) stays readable.
fn value_to_string(row: &rusqlite::Row, idx: usize) -> String {
    match row.get_ref(idx) {
        Ok(ValueRef::Null) => "NULL".into(),
        Ok(ValueRef::Integer(i)) => i.to_string(),
        Ok(ValueRef::Real(f)) => f.to_string(),
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
        Ok(ValueRef::Blob(b)) => format!("<blob: {} bytes>", b.len()),
        Err(_) => "<error>".into(),
    }
}
