//! Database admin tooling for the `/admin/database` page.
//!
//! The page browses two kinds of database behind a single UI:
//! - **Internal SQLite** — the app's own `main.db` (accounts, sessions,
//!   images, …) and `requests.db` (the HTTP request log). These are always
//!   available.
//! - **External PostgreSQL** — the optional instance pointed at by
//!   `postgres_url`. Only listed when that config key is set.
//!
//! Each database is addressed by an opaque *source id* of the form
//! `sqlite:<name>` or `pg:<dbname>` (see [`Source`]). The dispatch helpers
//! below parse that id and forward to the right backend submodule, so the
//! route layer and the frontend never have to special-case a backend.
//!
//! Safety:
//! - Safe-mode is the default. The query is first screened by the
//!   text-level [`is_safe_query`] prefilter (rejects obvious writes with a
//!   friendly message), then the backend enforces read-only-ness at the
//!   engine level — Postgres via `BEGIN TRANSACTION READ ONLY`, SQLite via
//!   `PRAGMA query_only = ON`.
//! - Danger mode skips both layers and is only reachable through an
//!   admin-only checkbox plus an explicit confirmation in the UI.

pub mod postgres;
mod safety;
pub mod sqlite;

pub use safety::is_safe_query;

use serde::Serialize;

use crate::AppState;

/// Hard cap on the number of rows returned by the query runner so a
/// `SELECT * FROM big_table` doesn't OOM the browser tab. Shared by both
/// backends.
pub const ROW_LIMIT: usize = 1000;

// ─── Shared catalog/result shapes ────────────────────────────────────

/// One entry in the database picker.
#[derive(Debug, Serialize)]
pub struct DatabaseInfo {
    /// Opaque source id (`"sqlite:main"`, `"pg:postgres"`, …). This is what
    /// the frontend sends back to the table/query endpoints.
    pub id: String,
    /// Display name (`"main"`, `"requests"`, or the Postgres database name).
    pub name: String,
    /// Backend kind — `"sqlite"` or `"postgres"`. The UI uses this to hide
    /// Postgres-only features (e.g. the Roles tab) for SQLite sources.
    pub kind: &'static str,
    pub owner: String,
    pub encoding: String,
    /// On-disk size as a human string (`"42 MB"`).
    pub size_pretty: String,
}

#[derive(Debug, Serialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub owner: String,
    pub row_estimate: i64,
    pub size_pretty: String,
}

#[derive(Debug, Serialize)]
pub struct RoleInfo {
    pub name: String,
    pub superuser: bool,
    pub can_login: bool,
    pub can_create_db: bool,
    pub can_create_role: bool,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub row_count: usize,
    pub elapsed_ms: u64,
    /// `true` when we capped the result set at [`ROW_LIMIT`]. The UI shows a
    /// banner so the operator knows results are partial.
    pub truncated: bool,
}

// ─── Source addressing ───────────────────────────────────────────────

/// A parsed database source id.
pub enum Source {
    /// One of the internal SQLite databases.
    Sqlite(sqlite::Which),
    /// An external Postgres database by name.
    Postgres(String),
}

/// Parses an opaque source id (`"sqlite:main"`, `"pg:foo"`) into a [`Source`].
pub fn parse_source(id: &str) -> anyhow::Result<Source> {
    if let Some(name) = id.strip_prefix("sqlite:") {
        Ok(Source::Sqlite(sqlite::Which::parse(name)?))
    } else if let Some(db) = id.strip_prefix("pg:") {
        if db.is_empty() {
            anyhow::bail!("missing Postgres database name in source id");
        }
        Ok(Source::Postgres(db.to_string()))
    } else {
        anyhow::bail!("unknown database source: {id}")
    }
}

// ─── Dispatch ────────────────────────────────────────────────────────

/// Lists every browsable database: the internal SQLite ones first, then the
/// Postgres databases when `postgres_url` is configured.
///
/// A failure reaching Postgres is logged and swallowed — the internal
/// databases are always available, so a down/misconfigured Postgres must not
/// take the whole page with it.
pub async fn list_databases(state: &AppState) -> anyhow::Result<Vec<DatabaseInfo>> {
    let mut out = sqlite::list_databases(state).await?;
    if state.config().postgres_url.is_some() {
        match postgres::list_databases(state).await {
            Ok(dbs) => out.extend(dbs),
            Err(e) => tracing::warn!(error = %e, "could not list Postgres databases"),
        }
    }
    Ok(out)
}

/// Lists the tables in the database identified by `source`.
pub async fn list_tables(state: &AppState, source: &str) -> anyhow::Result<Vec<TableInfo>> {
    match parse_source(source)? {
        Source::Sqlite(which) => sqlite::list_tables(state, which).await,
        Source::Postgres(db) => postgres::list_tables(state, &db).await,
    }
}

/// Lists Postgres roles. SQLite has no role system, so this is only
/// meaningful for Postgres sources.
pub async fn list_roles(state: &AppState) -> anyhow::Result<Vec<RoleInfo>> {
    postgres::list_roles(state).await
}

/// Runs `sql` against the database identified by `source`. When `safe` is
/// true the backend enforces read-only execution at the engine level.
pub async fn run_query(state: &AppState, source: &str, sql: &str, safe: bool) -> anyhow::Result<QueryResult> {
    match parse_source(source)? {
        Source::Sqlite(which) => sqlite::run_query(state, which, sql, safe).await,
        Source::Postgres(db) => postgres::run_query(state, &db, sql, safe).await,
    }
}
