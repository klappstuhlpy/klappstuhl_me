use std::collections::HashMap;

use askama::Template;
use axum::{
    extract::{Query, State},
    response::Redirect,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::database::Table;
use crate::{audit::AuditLogEntry, error::ApiError, models::Account, AppState};

#[derive(Debug, Deserialize)]
struct AuditLogQuery {
    #[serde(default)]
    account_id: Option<i64>,
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    after: Option<i64>,
}

impl AuditLogQuery {
    fn to_sql(&self) -> (String, Vec<i64>) {
        let mut filters = Vec::new();
        let mut params = Vec::new();
        if let Some(account_id) = self.account_id {
            filters.push("audit_log.account_id = ?");
            params.push(account_id);
        }
        if let Some(before) = self.before {
            filters.push("audit_log.id < ?");
            params.push(before);
        }
        if let Some(after) = self.after {
            filters.push("audit_log.id > ?");
            params.push(after);
        }

        if filters.is_empty() {
            (String::new(), params)
        } else {
            (filters.join(" AND "), params)
        }
    }
}

#[derive(Debug, Default, Serialize)]
struct AuditLogResult {
    logs: Vec<AuditLogEntry>,
    users: HashMap<i64, String>,
}

async fn get_audit_logs(
    State(state): State<AppState>,
    Query(query): Query<AuditLogQuery>,
    account: Account,
) -> Result<Json<AuditLogResult>, ApiError> {
    if !account.flags.is_admin() {
        return Err(ApiError::forbidden());
    }

    let (filter, params) = query.to_sql();
    let mut query = r###"
        SELECT
            audit_log.*,
            account.name AS account_name
        FROM audit_log
        LEFT JOIN account ON account.id = audit_log.account_id
    "###
    .to_owned();

    if !filter.is_empty() {
        query.push_str("WHERE ");
        query.push_str(&filter);
    }
    query.push_str(" ORDER BY audit_log.id DESC LIMIT 100");

    let result = state
        .database()
        .call(move |connection| -> rusqlite::Result<_> {
            let mut result = AuditLogResult::default();
            let mut stmt = connection.prepare_cached(&query)?;
            let mut rows = stmt.query(rusqlite::params_from_iter(params))?;

            while let Some(row) = rows.next()? {
                let log = AuditLogEntry::from_row(row)?;

                if let Some(account_id) = log.account_id.clone() {
                    result.users.insert(account_id, row.get("account_name")?);
                }
                result.logs.push(log);
            }
            Ok(result)
        })
        .await?;

    Ok(Json(result))
}

#[derive(Template)]
#[template(path = "audit.html")]
struct AuditLogTemplate {
    account: Option<Account>,
}

async fn logs(account: Account) -> Result<AuditLogTemplate, Redirect> {
    if !account.flags.is_admin() {
        Err(Redirect::to("/"))
    } else {
        Ok(AuditLogTemplate { account: Some(account) })
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/audit-logs", get(get_audit_logs))
        .route("/logs", get(logs))
}
