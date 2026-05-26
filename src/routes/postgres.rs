//! `/admin/postgres` routes — list DBs/tables/users + safe-mode query
//! runner. Every query writes an audit entry.

use crate::{
    headers::ClientIp,
    models::Account,
    postgres,
    AppState,
};
use askama::Template;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Form, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Template)]
#[template(path = "admin_postgres.html")]
struct AdminPostgresTemplate {
    account: Option<Account>,
    active_page: &'static str,
    /// `true` when `postgres_url` is set in config. When false the
    /// page renders a static "not configured" notice and skips the
    /// data loaders.
    configured: bool,
}

async fn postgres_page(
    State(state): State<AppState>,
    account: Account,
) -> Result<AdminPostgresTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminPostgresTemplate {
        account: Some(account),
        active_page: "postgres",
        configured: state.config().postgres_url.is_some(),
    })
}

// Helper that gates every JSON endpoint on (admin + postgres configured).
fn check_admin_and_pg(state: &AppState, account: &Account) -> Result<(), StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    if state.config().postgres_url.is_none() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(())
}

#[derive(Serialize)]
struct ApiError {
    error: String,
}

fn api_error(e: impl ToString) -> (StatusCode, Json<ApiError>) {
    (StatusCode::BAD_GATEWAY, Json(ApiError { error: e.to_string() }))
}

// ─── Catalog endpoints ───────────────────────────────────────────────

async fn list_databases(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<Vec<postgres::DatabaseInfo>>, (StatusCode, Json<ApiError>)> {
    check_admin_and_pg(&state, &account).map_err(|s| (s, Json(ApiError { error: "forbidden".into() })))?;
    postgres::list_databases(&state).await.map(Json).map_err(api_error)
}

#[derive(Deserialize)]
struct DbQuery {
    db: String,
}

async fn list_tables(
    State(state): State<AppState>,
    account: Account,
    Query(q): Query<DbQuery>,
) -> Result<Json<Vec<postgres::TableInfo>>, (StatusCode, Json<ApiError>)> {
    check_admin_and_pg(&state, &account).map_err(|s| (s, Json(ApiError { error: "forbidden".into() })))?;
    postgres::list_tables(&state, &q.db).await.map(Json).map_err(api_error)
}

async fn list_roles(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<Vec<postgres::RoleInfo>>, (StatusCode, Json<ApiError>)> {
    check_admin_and_pg(&state, &account).map_err(|s| (s, Json(ApiError { error: "forbidden".into() })))?;
    postgres::list_roles(&state).await.map(Json).map_err(api_error)
}

// ─── Query runner ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RunQuery {
    db: String,
    sql: String,
    /// When true, bypass safe-mode and run in a normal transaction.
    /// Only honoured for admins (already gated above) — the UI also
    /// requires an explicit confirmation click.
    #[serde(default)]
    danger_mode: bool,
}

async fn run_query(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Form(payload): Form<RunQuery>,
) -> Result<Json<postgres::QueryResult>, (StatusCode, Json<ApiError>)> {
    check_admin_and_pg(&state, &account).map_err(|s| (s, Json(ApiError { error: "forbidden".into() })))?;

    // Safe-mode pre-flight rejection — keep error messages obvious.
    let safe = !payload.danger_mode;
    if safe && !postgres::is_safe_query(&payload.sql) {
        // Audit the *attempt* so misuse is visible.
        state
            .audit("postgres.query.blocked")
            .actor(&account)
            .target(payload.db.clone())
            .ip_opt(client_ip)
            .meta(serde_json::json!({
                "reason": "non_read_in_safe_mode",
                "sql_prefix": snippet(&payload.sql),
            }))
            .fire();
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: "Blocked by safe-mode: only SELECT / EXPLAIN / SHOW / WITH / VALUES / TABLE / FETCH allowed.".into(),
            }),
        ));
    }

    // Run it.
    let outcome = postgres::run_query(&state, &payload.db, &payload.sql, safe).await;

    // Audit (success or failure both get recorded).
    let mut meta = serde_json::json!({
        "sql_prefix": snippet(&payload.sql),
        "danger_mode": payload.danger_mode,
    });
    let action = match &outcome {
        Ok(qr) => {
            meta["row_count"] = qr.row_count.into();
            meta["elapsed_ms"] = qr.elapsed_ms.into();
            "postgres.query"
        }
        Err(e) => {
            meta["error"] = e.to_string().into();
            "postgres.query.error"
        }
    };
    state
        .audit(action)
        .actor(&account)
        .target(payload.db.clone())
        .ip_opt(client_ip)
        .meta(meta)
        .fire();

    outcome.map(Json).map_err(api_error)
}

/// Truncates `sql` so the audit row stays small. Newlines collapsed to spaces.
fn snippet(sql: &str) -> String {
    let collapsed: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() > 200 {
        format!("{}…", &collapsed[..200])
    } else {
        collapsed
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/postgres", get(postgres_page))
        .route("/admin/postgres/databases", get(list_databases))
        .route("/admin/postgres/tables", get(list_tables))
        .route("/admin/postgres/roles", get(list_roles))
        .route("/admin/postgres/query", post(run_query))
}
