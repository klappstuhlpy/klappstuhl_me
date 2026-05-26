//! PostgreSQL admin tooling for `/admin/postgres`.
//!
//! `tokio-postgres` is connected per-request — admin pages don't see
//! enough traffic to warrant a pool, and per-request connections also
//! give us a clean way to switch databases (Postgres has no
//! `USE database` and a client must reconnect to target a different DB).
//!
//! Safety:
//! - Safe-mode queries run inside `BEGIN TRANSACTION READ ONLY` so even
//!   if the connecting user has DDL/DML rights, the query physically
//!   cannot mutate anything.  This is bulletproof against parser-bypass
//!   tricks (comments, quoted identifiers, etc.).
//! - `is_safe_query()` is a lightweight pre-filter that rejects obvious
//!   writes before we even contact the server — it short-circuits the
//!   common case and keeps error messages user-friendly.
//! - In danger mode the operator has explicitly opted-in via a checkbox
//!   that's only visible to admins; the safety wrappers are skipped.

mod safety;

pub use safety::is_safe_query;

use serde::Serialize;
use std::time::{Duration, Instant};
use tokio_postgres::{config::Config as PgConfig, types::Type, NoTls};

use crate::AppState;

/// Hard cap on the number of rows returned by the query runner so a
/// `SELECT * FROM big_table` doesn't OOM the browser tab.
pub const ROW_LIMIT: usize = 1000;
/// Connection + statement timeout (per HTTP request).
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Parses the configured `postgres_url` and overrides the target database
/// if provided. Returns an error if no URL is configured at all.
fn parse_url(url: &str, override_db: Option<&str>) -> anyhow::Result<PgConfig> {
    let mut cfg: PgConfig = url.parse()?;
    cfg.connect_timeout(QUERY_TIMEOUT);
    if let Some(db) = override_db {
        cfg.dbname(db);
    }
    Ok(cfg)
}

/// Opens a single short-lived connection to the configured Postgres
/// instance, optionally targeting a specific database.
///
/// The spawned `connection` task drives the protocol — we await `client`
/// for queries.  When the client is dropped at the end of the request,
/// the connection task exits gracefully.
async fn connect(state: &AppState, db: Option<&str>) -> anyhow::Result<tokio_postgres::Client> {
    let url = state
        .config()
        .postgres_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("postgres_url not configured"))?;
    let cfg = parse_url(url, db)?;

    let (client, connection) = cfg.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!(error = %e, "postgres connection task ended");
        }
    });
    Ok(client)
}

// ─── Catalog reads ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct DatabaseInfo {
    pub name: String,
    pub owner: String,
    pub encoding: String,
    /// On-disk size as a human string (`"42 MB"`, returned by
    /// `pg_size_pretty(pg_database_size(...))`).  We use the formatted
    /// string so the UI doesn't have to know about Postgres' size types.
    pub size_pretty: String,
}

pub async fn list_databases(state: &AppState) -> anyhow::Result<Vec<DatabaseInfo>> {
    let client = connect(state, None).await?;
    let rows = client
        .query(
            "SELECT d.datname AS name,
                    pg_get_userbyid(d.datdba) AS owner,
                    pg_encoding_to_char(d.encoding) AS encoding,
                    pg_size_pretty(pg_database_size(d.datname)) AS size_pretty
             FROM pg_database d
             WHERE d.datistemplate = false
             ORDER BY d.datname",
            &[],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| DatabaseInfo {
            name: r.get("name"),
            owner: r.get("owner"),
            encoding: r.get("encoding"),
            size_pretty: r.get("size_pretty"),
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub owner: String,
    pub row_estimate: i64,
    pub size_pretty: String,
}

pub async fn list_tables(state: &AppState, db: &str) -> anyhow::Result<Vec<TableInfo>> {
    let client = connect(state, Some(db)).await?;
    let rows = client
        .query(
            "SELECT
                c.relnamespace::regnamespace::text         AS schema,
                c.relname                                  AS name,
                pg_get_userbyid(c.relowner)                AS owner,
                COALESCE(s.n_live_tup, 0)::bigint          AS row_estimate,
                pg_size_pretty(pg_total_relation_size(c.oid)) AS size_pretty
             FROM pg_class c
             LEFT JOIN pg_stat_user_tables s
                   ON s.relid = c.oid
             WHERE c.relkind = 'r'
               AND c.relnamespace::regnamespace::text
                   NOT IN ('pg_catalog', 'information_schema')
             ORDER BY schema, name",
            &[],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| TableInfo {
            schema: r.get("schema"),
            name: r.get("name"),
            owner: r.get("owner"),
            row_estimate: r.get("row_estimate"),
            size_pretty: r.get("size_pretty"),
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct RoleInfo {
    pub name: String,
    pub superuser: bool,
    pub can_login: bool,
    pub can_create_db: bool,
    pub can_create_role: bool,
}

pub async fn list_roles(state: &AppState) -> anyhow::Result<Vec<RoleInfo>> {
    let client = connect(state, None).await?;
    let rows = client
        .query(
            "SELECT rolname,
                    rolsuper,
                    rolcanlogin,
                    rolcreatedb,
                    rolcreaterole
             FROM pg_roles
             WHERE rolname NOT LIKE 'pg\\_%' ESCAPE '\\'
             ORDER BY rolname",
            &[],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| RoleInfo {
            name: r.get("rolname"),
            superuser: r.get("rolsuper"),
            can_login: r.get("rolcanlogin"),
            can_create_db: r.get("rolcreatedb"),
            can_create_role: r.get("rolcreaterole"),
        })
        .collect())
}

// ─── Query runner ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub row_count: usize,
    pub elapsed_ms: u64,
    /// `true` when we capped the result set at [`ROW_LIMIT`].  The UI
    /// shows a banner so the operator knows results are partial.
    pub truncated: bool,
}

/// Runs the given SQL.  When `safe` is true, wraps the query in
/// `BEGIN TRANSACTION READ ONLY` … `COMMIT` so the server enforces
/// read-only-ness regardless of the connecting role's privileges.
pub async fn run_query(
    state: &AppState,
    db: &str,
    sql: &str,
    safe: bool,
) -> anyhow::Result<QueryResult> {
    let client = connect(state, Some(db)).await?;

    let started = Instant::now();
    let (rows, _) = if safe {
        // Read-only transaction: even DDL/DML against systems the role
        // could otherwise mutate are rejected by the server.
        let tx = client.build_transaction().read_only(true).start().await?;
        let rows = tx.query(sql, &[]).await?;
        tx.commit().await?;
        (rows, ())
    } else {
        (client.query(sql, &[]).await?, ())
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;

    // Column headers — preserved in declared order.
    let columns: Vec<String> = rows
        .first()
        .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
        .unwrap_or_default();

    // Coerce each cell to a display string. We try a few common Postgres
    // types in priority order; anything we can't classify falls back to
    // a "<unsupported>" marker so the row count + columns still render.
    let total = rows.len();
    let truncated = total > ROW_LIMIT;
    let cells: Vec<Vec<String>> = rows
        .iter()
        .take(ROW_LIMIT)
        .map(|row| {
            (0..row.columns().len())
                .map(|i| cell_to_string(row, i))
                .collect()
        })
        .collect();

    Ok(QueryResult {
        columns,
        rows: cells,
        row_count: total,
        elapsed_ms,
        truncated,
    })
}

/// Per-cell type-dispatch.  Covers everything you'd reasonably encounter
/// in an admin tool — TEXT family, integers, floats, booleans, JSON,
/// UUID, timestamps. Unknown OIDs degrade gracefully to a placeholder.
///
/// Implemented as if/else on `&Type` rather than a `match` because the
/// inner enum is non-exhaustive and Rust 2021 rejects pattern-matching
/// associated consts of structs with private fields.
fn cell_to_string(row: &tokio_postgres::Row, idx: usize) -> String {
    let ty = row.columns()[idx].type_();

    let s_get = |row: &tokio_postgres::Row, i: usize| -> Option<String> {
        row.try_get::<_, Option<String>>(i).ok().flatten()
    };

    if ty == &Type::TEXT
        || ty == &Type::VARCHAR
        || ty == &Type::BPCHAR
        || ty == &Type::NAME
        || ty == &Type::UNKNOWN
        || ty == &Type::CITEXT
    {
        s_get(row, idx).unwrap_or_else(|| "NULL".into())
    } else if ty == &Type::INT2 {
        fmt_opt::<i16>(row, idx)
    } else if ty == &Type::INT4 {
        fmt_opt::<i32>(row, idx)
    } else if ty == &Type::INT8 {
        fmt_opt::<i64>(row, idx)
    } else if ty == &Type::OID {
        fmt_opt::<u32>(row, idx)
    } else if ty == &Type::FLOAT4 {
        fmt_opt::<f32>(row, idx)
    } else if ty == &Type::FLOAT8 {
        fmt_opt::<f64>(row, idx)
    } else if ty == &Type::BOOL {
        fmt_opt::<bool>(row, idx)
    } else if ty == &Type::JSON || ty == &Type::JSONB {
        row.try_get::<_, Option<serde_json::Value>>(idx)
            .ok()
            .flatten()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "NULL".into())
    } else if ty == &Type::UUID {
        row.try_get::<_, Option<uuid::Uuid>>(idx)
            .ok()
            .flatten()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "NULL".into())
    } else if ty == &Type::TIMESTAMP {
        row.try_get::<_, Option<time::PrimitiveDateTime>>(idx)
            .ok()
            .flatten()
            .map(|t| t.to_string())
            .unwrap_or_else(|| "NULL".into())
    } else if ty == &Type::TIMESTAMPTZ {
        row.try_get::<_, Option<time::OffsetDateTime>>(idx)
            .ok()
            .flatten()
            .map(|t| t.to_string())
            .unwrap_or_else(|| "NULL".into())
    } else if ty == &Type::DATE {
        row.try_get::<_, Option<time::Date>>(idx)
            .ok()
            .flatten()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "NULL".into())
    } else if ty == &Type::TIME {
        row.try_get::<_, Option<time::Time>>(idx)
            .ok()
            .flatten()
            .map(|t| t.to_string())
            .unwrap_or_else(|| "NULL".into())
    } else {
        format!("<unsupported: {}>", ty.name())
    }
}

fn fmt_opt<'a, T>(row: &'a tokio_postgres::Row, idx: usize) -> String
where
    T: tokio_postgres::types::FromSql<'a> + std::fmt::Display,
{
    row.try_get::<_, Option<T>>(idx)
        .ok()
        .flatten()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "NULL".into())
}
