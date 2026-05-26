//! SSH key management admin routes.
//!
//! GET    /admin/ssh                          — keys + tokens page
//! GET    /admin/ssh/data                     — JSON key list + stats
//! POST   /admin/ssh/keys                     — add a key (JSON)
//! POST   /admin/ssh/keys/:id/revoke          — revoke key
//! DELETE /admin/ssh/keys/:id                 — delete key (hard)
//!
//! GET    /admin/ssh/tokens                   — JSON token list (no plaintext)
//! POST   /admin/ssh/tokens                   — issue token (plaintext returned ONCE)
//! POST   /admin/ssh/tokens/:id/revoke        — revoke token
//! DELETE /admin/ssh/tokens/:id               — delete token (hard)
//!
//! GET    /admin/ssh/audit                    — SSH session audit page
//! GET    /admin/ssh/audit/data               — JSON audit entries (filterable)
//! GET    /admin/ssh/export/authorized_keys   — download active keys as authorized_keys

use crate::{
    boxed_params,
    database::{is_unique_constraint_violation, Table},
    headers::ClientIp,
    models::Account,
    ssh::{self, SshKey, SshSessionAudit, SshToken},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};

// ─── Page ────────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin_ssh.html")]
struct AdminSshTemplate {
    account: Option<Account>,
    active_page: &'static str,
}

async fn ssh_page(account: Account) -> Result<AdminSshTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminSshTemplate {
        account: Some(account),
        active_page: "ssh",
    })
}

// ─── Data endpoint ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SshData {
    keys: Vec<SshKey>,
    total: usize,
    active: usize,
    revoked: usize,
}

async fn ssh_data(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<SshData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let keys: Vec<SshKey> = state
        .database()
        .all(
            "SELECT id, account_id, name, public_key, fingerprint, algo, comment,
                    added_at, last_used_at, revoked_at
             FROM ssh_key ORDER BY added_at DESC",
            [],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let active = keys.iter().filter(|k| k.is_active()).count();
    let revoked = keys.len() - active;

    Ok(Json(SshData {
        total: keys.len(),
        active,
        revoked,
        keys,
    }))
}

// ─── Add key ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AddKeyPayload {
    name: String,
    public_key: String,
}

#[derive(Serialize)]
struct AddKeyResponse {
    id: i64,
    fingerprint: String,
    algo: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn add_key(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<AddKeyPayload>,
) -> Result<(StatusCode, Json<AddKeyResponse>), (StatusCode, Json<ErrorResponse>)> {
    if !account.flags.is_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse { error: "forbidden".into() }),
        ));
    }

    let name = payload.name.trim().to_owned();
    if name.is_empty() || name.len() > 100 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse { error: "name must be 1–100 characters".into() }),
        ));
    }

    let raw_key = payload.public_key.trim().to_owned();
    let parsed = ssh::parse_public_key(&raw_key).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse { error: format!("invalid SSH key: {e}") }),
        )
    })?;

    let fingerprint = parsed.fingerprint.clone();
    let algo = parsed.algo.clone();
    let comment = parsed.comment.clone();
    let account_id = account.id;

    let result = state
        .database()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO ssh_key(account_id, name, public_key, fingerprint, algo, comment)
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![account_id, name, raw_key, fingerprint, algo, comment],
            )
            .map(|_| conn.last_insert_rowid())
        })
        .await;

    match result {
        Ok(id) => {
            ssh::audit(
                &state,
                Some(account.id),
                Some(id),
                "ssh.key.add",
                client_ip.map(|ip| ip.to_string()),
                None,
            );
            state
                .audit("ssh.key.add")
                .actor(&account)
                .target(parsed.fingerprint.clone())
                .ip_opt(client_ip)
                .meta(serde_json::json!({ "algo": parsed.algo }))
                .fire();
            Ok((
                StatusCode::CREATED,
                Json(AddKeyResponse {
                    id,
                    fingerprint: parsed.fingerprint,
                    algo: parsed.algo,
                }),
            ))
        }
        Err(e) if is_unique_constraint_violation(&e) => Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "a key with this fingerprint already exists for this account".into(),
            }),
        )),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: "database error".into() }),
        )),
    }
}

// ─── Revoke key ───────────────────────────────────────────────────────────────

async fn revoke_key(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let rows = state
        .database()
        .execute(
            "UPDATE ssh_key SET revoked_at = CURRENT_TIMESTAMP
             WHERE id = ? AND revoked_at IS NULL",
            boxed_params!(id),
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    ssh::audit(
        &state,
        Some(account.id),
        Some(id),
        "ssh.key.revoke",
        client_ip.map(|ip| ip.to_string()),
        None,
    );
    state
        .audit("ssh.key.revoke")
        .actor(&account)
        .target(format!("key:{id}"))
        .ip_opt(client_ip)
        .fire();

    Ok(StatusCode::NO_CONTENT)
}

// ─── Delete key ───────────────────────────────────────────────────────────────

async fn delete_key(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let rows = state
        .database()
        .execute("DELETE FROM ssh_key WHERE id = ?", boxed_params!(id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    ssh::audit(
        &state,
        Some(account.id),
        None,
        "ssh.key.delete",
        client_ip.map(|ip| ip.to_string()),
        None,
    );
    state
        .audit("ssh.key.delete")
        .actor(&account)
        .target(format!("key:{id}"))
        .ip_opt(client_ip)
        .fire();

    Ok(StatusCode::NO_CONTENT)
}

// ─── Token list ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct TokensData {
    tokens: Vec<SshToken>,
    total: usize,
    active: usize,
}

async fn list_tokens(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TokensData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let tokens: Vec<SshToken> = state
        .database()
        .all(
            "SELECT id, account_id, token_hash, label, scopes,
                    expires_at, created_at, used_at, revoked_at
             FROM ssh_token ORDER BY created_at DESC",
            [],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let active = tokens.iter().filter(|t| t.is_active()).count();

    Ok(Json(TokensData {
        total: tokens.len(),
        active,
        tokens,
    }))
}

// ─── Issue token ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct IssueTokenPayload {
    label: String,
    /// Comma-separated scopes, or empty for full access.
    #[serde(default)]
    scopes: String,
    /// Expiry in hours from now. None / 0 = never expires.
    expires_in_hours: Option<i64>,
}

#[derive(Serialize)]
struct IssueTokenResponse {
    id: i64,
    /// Plaintext token — shown ONCE, never stored.
    token: String,
    label: String,
    expires_at: Option<String>,
}

async fn issue_token(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<IssueTokenPayload>,
) -> Result<(StatusCode, Json<IssueTokenResponse>), (StatusCode, Json<ErrorResponse>)> {
    if !account.flags.is_admin() {
        return Err((StatusCode::FORBIDDEN, Json(ErrorResponse { error: "forbidden".into() })));
    }

    let label = payload.label.trim().to_owned();
    if label.is_empty() || label.len() > 100 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse { error: "label must be 1–100 characters".into() }),
        ));
    }

    let plaintext = ssh::generate_token();
    let token_hash = ssh::hash_token(&plaintext);
    let scopes = payload.scopes.trim().to_owned();
    let account_id = account.id;

    let expires_at: Option<time::OffsetDateTime> = payload
        .expires_in_hours
        .filter(|&h| h > 0)
        .map(|h| time::OffsetDateTime::now_utc() + time::Duration::hours(h));

    let expires_at_str = expires_at.as_ref().map(|dt| {
        dt.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    });

    let expires_at_db = expires_at_str.clone();
    let label_db = label.clone();
    let scopes_db = scopes.clone();

    let result = state
        .database()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO ssh_token(account_id, token_hash, label, scopes, expires_at)
                 VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![account_id, token_hash, label_db, scopes_db, expires_at_db],
            )
            .map(|_| conn.last_insert_rowid())
        })
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "database error".into() })))?;

    ssh::audit(
        &state,
        Some(account.id),
        None,
        "ssh.token.issue",
        client_ip.map(|ip| ip.to_string()),
        None,
    );
    state
        .audit("ssh.token.issue")
        .actor(&account)
        .target(format!("token:{result}"))
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "label": label, "expires_in_hours": payload.expires_in_hours }))
        .fire();

    Ok((
        StatusCode::CREATED,
        Json(IssueTokenResponse {
            id: result,
            token: plaintext,
            label,
            expires_at: expires_at_str,
        }),
    ))
}

// ─── Revoke token ─────────────────────────────────────────────────────────────

async fn revoke_token(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let rows = state
        .database()
        .execute(
            "UPDATE ssh_token SET revoked_at = CURRENT_TIMESTAMP
             WHERE id = ? AND revoked_at IS NULL",
            boxed_params!(id),
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    ssh::audit(&state, Some(account.id), None, "ssh.token.revoke",
        client_ip.map(|ip| ip.to_string()), None);
    state.audit("ssh.token.revoke").actor(&account)
        .target(format!("token:{id}")).ip_opt(client_ip).fire();

    Ok(StatusCode::NO_CONTENT)
}

// ─── Delete token ─────────────────────────────────────────────────────────────

async fn delete_token(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let rows = state
        .database()
        .execute("DELETE FROM ssh_token WHERE id = ?", boxed_params!(id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    ssh::audit(&state, Some(account.id), None, "ssh.token.delete",
        client_ip.map(|ip| ip.to_string()), None);
    state.audit("ssh.token.delete").actor(&account)
        .target(format!("token:{id}")).ip_opt(client_ip).fire();

    Ok(StatusCode::NO_CONTENT)
}

// ─── SSH audit page ───────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin_ssh_audit.html")]
struct AdminSshAuditTemplate {
    account: Option<Account>,
    active_page: &'static str,
}

async fn ssh_audit_page(account: Account) -> Result<AdminSshAuditTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminSshAuditTemplate {
        account: Some(account),
        active_page: "ssh",
    })
}

// ─── SSH audit data ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SshAuditQuery {
    /// Filter by key ID.
    key_id: Option<i64>,
    /// Filter by action prefix (e.g. "ssh.key").
    #[serde(default)]
    action: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 { 200 }

#[derive(Serialize)]
struct SshAuditData {
    entries: Vec<SshSessionAudit>,
    total: usize,
}

async fn ssh_audit_data(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<SshAuditQuery>,
) -> Result<Json<SshAuditData>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let key_id = query.key_id;
    let action_filter = query.action.filter(|s| !s.is_empty()).map(|s| format!("{s}%"));
    let limit = query.limit.clamp(1, 500);

    let entries: Vec<SshSessionAudit> = state
        .database()
        .call(move |conn| {
            let mut sql = "SELECT id, account_id, key_id, action, ip, user_agent, created_at
                           FROM ssh_session_audit WHERE 1=1".to_string();
            if key_id.is_some()      { sql.push_str(" AND key_id = ?"); }
            if action_filter.is_some() { sql.push_str(" AND action LIKE ?"); }
            sql.push_str(" ORDER BY id DESC LIMIT ?");

            let mut stmt = conn.prepare_cached(&sql)?;
            let rows: rusqlite::Result<Vec<SshSessionAudit>> = match (key_id, action_filter) {
                (Some(k), Some(a)) => stmt
                    .query_map(rusqlite::params![k, a, limit], SshSessionAudit::from_row)?
                    .collect(),
                (Some(k), None) => stmt
                    .query_map(rusqlite::params![k, limit], SshSessionAudit::from_row)?
                    .collect(),
                (None, Some(a)) => stmt
                    .query_map(rusqlite::params![a, limit], SshSessionAudit::from_row)?
                    .collect(),
                (None, None) => stmt
                    .query_map(rusqlite::params![limit], SshSessionAudit::from_row)?
                    .collect(),
            };
            rows
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total = entries.len();
    Ok(Json(SshAuditData { entries, total }))
}

// ─── Export authorized_keys ───────────────────────────────────────────────────

async fn export_authorized_keys(
    State(state): State<AppState>,
    account: Account,
) -> Result<Response, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    let keys: Vec<SshKey> = state
        .database()
        .all(
            "SELECT id, account_id, name, public_key, fingerprint, algo, comment,
                    added_at, last_used_at, revoked_at
             FROM ssh_key WHERE revoked_at IS NULL ORDER BY added_at ASC",
            [],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Build standard authorized_keys content — one key per line, with a
    // comment noting the label so the admin can trace each line back.
    let mut body = String::from("# Generated by klappstuhl.me — do not edit manually\n");
    for key in &keys {
        body.push_str(&format!(
            "# {} (added {})\n{}\n",
            key.name,
            key.added_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            key.public_key.trim(),
        ));
    }

    state
        .audit("ssh.export.authorized_keys")
        .actor(&account)
        .meta(serde_json::json!({ "key_count": keys.len() }))
        .fire();

    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"authorized_keys\"",
            ),
        ],
        body,
    )
        .into_response())
}

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/ssh", get(ssh_page))
        .route("/admin/ssh/data", get(ssh_data))
        .route("/admin/ssh/keys", post(add_key))
        .route("/admin/ssh/keys/:id/revoke", post(revoke_key))
        .route("/admin/ssh/keys/:id", delete(delete_key))
        .route("/admin/ssh/tokens", get(list_tokens).post(issue_token))
        .route("/admin/ssh/tokens/:id/revoke", post(revoke_token))
        .route("/admin/ssh/tokens/:id", delete(delete_token))
        .route("/admin/ssh/audit", get(ssh_audit_page))
        .route("/admin/ssh/audit/data", get(ssh_audit_data))
        .route("/admin/ssh/export/authorized_keys", get(export_authorized_keys))
}
