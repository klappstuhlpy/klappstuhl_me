//! File sanitizer admin routes.
//!
//! GET    /admin/sanitizer          — upload + history page
//! POST   /admin/sanitizer/scan     — multipart upload; runs ClamAV + VT checks
//! GET    /admin/sanitizer/history  — JSON scan history
//! DELETE /admin/sanitizer/:id      — delete a history entry

use std::time::Duration;

use crate::{boxed_params, database::Table, headers::ClientIp, models::Account, AppState};
use askama::Template;
use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt::Write as FmtWrite;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ─── Model ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct FileScan {
    id: i64,
    filename: String,
    file_size: i64,
    sha256: String,
    clamav_clean: Option<i64>,
    clamav_virus: Option<String>,
    vt_status: Option<String>,
    vt_positives: Option<i64>,
    vt_total: Option<i64>,
    vt_url: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    scanned_at: OffsetDateTime,
}

impl Table for FileScan {
    const NAME: &'static str = "file_scan";
    const COLUMNS: &'static [&'static str] = &[
        "id", "filename", "file_size", "sha256",
        "clamav_clean", "clamav_virus",
        "vt_status", "vt_positives", "vt_total", "vt_url",
        "scanned_at",
    ];
    type Id = i64;
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id:           row.get("id")?,
            filename:     row.get("filename")?,
            file_size:    row.get("file_size")?,
            sha256:       row.get("sha256")?,
            clamav_clean: row.get("clamav_clean")?,
            clamav_virus: row.get("clamav_virus")?,
            vt_status:    row.get("vt_status")?,
            vt_positives: row.get("vt_positives")?,
            vt_total:     row.get("vt_total")?,
            vt_url:       row.get("vt_url")?,
            scanned_at:   row.get("scanned_at")?,
        })
    }
}

// ─── Page ────────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "admin_sanitizer.html")]
struct AdminSanitizerTemplate {
    account: Option<Account>,
    active_page: &'static str,
    clamav_enabled: bool,
    vt_enabled: bool,
}

async fn sanitizer_page(
    State(state): State<AppState>,
    account: Account,
) -> Result<AdminSanitizerTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminSanitizerTemplate {
        account: Some(account),
        active_page: "sanitizer",
        clamav_enabled: state.config().clamav_addr.is_some(),
        vt_enabled: state.config().virustotal_api_key.is_some(),
    })
}

// ─── History ─────────────────────────────────────────────────────────────────

async fn history(State(state): State<AppState>, account: Account) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let scans: Vec<FileScan> = match state
        .database()
        .all(
            "SELECT id, filename, file_size, sha256, clamav_clean, clamav_virus,
                    vt_status, vt_positives, vt_total, vt_url, scanned_at
             FROM file_scan ORDER BY scanned_at DESC LIMIT 200",
            [],
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "history query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    Json(serde_json::json!({ "scans": scans })).into_response()
}

// ─── Delete ───────────────────────────────────────────────────────────────────

async fn delete_scan(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state
        .database()
        .execute("DELETE FROM file_scan WHERE id = ?", boxed_params![id])
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

// ─── Scan ────────────────────────────────────────────────────────────────────

async fn scan(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    mut multipart: Multipart,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    // ── Parse upload ─────────────────────────────────────────────────────────

    let (filename, data) = loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let name = field.name().unwrap_or("").to_owned();
                if name != "file" { continue; }
                let fname = field
                    .file_name()
                    .unwrap_or("upload")
                    .to_owned();
                match field.bytes().await {
                    Ok(b) => break (fname, b.to_vec()),
                    Err(e) => return (StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
                }
            }
            Ok(None) => return (StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "no file field in upload" }))).into_response(),
            Err(e) => return (StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
        }
    };

    let file_size = data.len() as i64;
    let sha256 = {
        let digest = Sha256::digest(&data);
        let mut s = String::with_capacity(64);
        for b in digest.iter() { write!(&mut s, "{b:02x}").unwrap(); }
        s
    };

    // ── ClamAV ───────────────────────────────────────────────────────────────

    let (clamav_clean, clamav_virus) =
        if let Some(addr) = state.config().clamav_addr.as_deref() {
            match tokio::time::timeout(Duration::from_secs(60), clamav_scan(addr, &data)).await {
                Ok(Ok(ClamResult::Clean)) => (Some(1i64), None),
                Ok(Ok(ClamResult::Found(v))) => (Some(0i64), Some(v)),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "ClamAV scan error");
                    (None, Some(format!("scan error: {e}")))
                }
                Err(_) => (None, Some("scan timed out".into())),
            }
        } else {
            (None, None)
        };

    // ── VirusTotal ───────────────────────────────────────────────────────────

    let (vt_status, vt_positives, vt_total, vt_url) =
        if let Some(key) = state.config().virustotal_api_key.as_deref() {
            match tokio::time::timeout(
                Duration::from_secs(30),
                vt_lookup(&state.client, key, &sha256),
            )
            .await
            {
                Ok(Ok(VtResult::Clean { positives, total, url })) =>
                    (Some("clean".to_owned()), Some(positives), Some(total), Some(url)),
                Ok(Ok(VtResult::Detected { positives, total, url })) =>
                    (Some("detected".to_owned()), Some(positives), Some(total), Some(url)),
                Ok(Ok(VtResult::Unknown)) =>
                    (Some("unknown".to_owned()), None, None, None),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "VirusTotal lookup error");
                    (Some("error".to_owned()), None, None, None)
                }
                Err(_) => (Some("error".to_owned()), None, None, None),
            }
        } else {
            (None, None, None, None)
        };

    // ── Persist ──────────────────────────────────────────────────────────────

    let row_id: i64 = match state
        .database()
        .get_row(
            "INSERT INTO file_scan (filename, file_size, sha256, clamav_clean, clamav_virus,
                                    vt_status, vt_positives, vt_total, vt_url)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
            boxed_params![
                filename.clone(),
                file_size,
                sha256.clone(),
                clamav_clean,
                clamav_virus.clone(),
                vt_status.clone(),
                vt_positives,
                vt_total,
                vt_url.clone()
            ],
            |row| row.get::<_, i64>(0),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "file_scan insert failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Derive a coarse overall verdict so the audit-log "meta" column has a
    // single field an operator can scan visually, plus the per-backend
    // detail underneath. Keys mirror the JSON returned to the client.
    let overall = if clamav_clean == Some(0) || vt_status.as_deref() == Some("detected") {
        "infected"
    } else if clamav_clean == Some(1) || vt_status.as_deref() == Some("clean") {
        "clean"
    } else {
        "unknown"
    };
    state.audit("sanitizer.scan")
        .actor(&account)
        .target(filename.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "verdict":      overall,
            "file_size":    file_size,
            "sha256":       sha256,
            "clamav_clean": clamav_clean,
            "clamav_virus": clamav_virus,
            "vt_status":    vt_status,
            "vt_positives": vt_positives,
            "vt_total":     vt_total,
        }))
        .fire();

    Json(serde_json::json!({
        "id": row_id,
        "filename": filename,
        "file_size": file_size,
        "sha256": sha256,
        "clamav_clean": clamav_clean,
        "clamav_virus": clamav_virus,
        "vt_status": vt_status,
        "vt_positives": vt_positives,
        "vt_total": vt_total,
        "vt_url": vt_url,
    })).into_response()
}

// ─── ClamAV client ────────────────────────────────────────────────────────────

enum ClamResult {
    Clean,
    Found(String),
}

async fn clamav_scan(addr: &str, data: &[u8]) -> anyhow::Result<ClamResult> {
    let mut sock = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| anyhow::anyhow!("connect to clamd ({addr}): {e}"))?;

    sock.write_all(b"zINSTREAM\0").await?;

    const CHUNK: usize = 65536;
    for chunk in data.chunks(CHUNK) {
        let len = chunk.len() as u32;
        sock.write_all(&len.to_be_bytes()).await?;
        sock.write_all(chunk).await?;
    }
    sock.write_all(&[0u8; 4]).await?;

    let mut resp = Vec::with_capacity(64);
    sock.read_to_end(&mut resp).await?;

    let resp = std::str::from_utf8(&resp)
        .unwrap_or("")
        .trim_end_matches('\0')
        .trim()
        .to_owned();

    if resp.ends_with(": OK") {
        Ok(ClamResult::Clean)
    } else if resp.ends_with(" FOUND") {
        let virus = resp
            .strip_prefix("stream: ")
            .and_then(|s| s.strip_suffix(" FOUND"))
            .unwrap_or(&resp)
            .to_owned();
        Ok(ClamResult::Found(virus))
    } else {
        Err(anyhow::anyhow!("unexpected clamd response: {resp}"))
    }
}

// ─── VirusTotal client ────────────────────────────────────────────────────────

enum VtResult {
    Clean    { positives: i64, total: i64, url: String },
    Detected { positives: i64, total: i64, url: String },
    Unknown,
}

async fn vt_lookup(
    client: &reqwest::Client,
    api_key: &str,
    sha256: &str,
) -> anyhow::Result<VtResult> {
    let url = format!("https://www.virustotal.com/api/v3/files/{sha256}");
    let resp = client
        .get(&url)
        .header("x-apikey", api_key)
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(VtResult::Unknown);
    }
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("VT API returned {}", resp.status()));
    }

    let json: serde_json::Value = resp.json().await?;
    let stats      = &json["data"]["attributes"]["last_analysis_stats"];
    let malicious  = stats["malicious"].as_i64().unwrap_or(0);
    let suspicious = stats["suspicious"].as_i64().unwrap_or(0);
    let harmless   = stats["harmless"].as_i64().unwrap_or(0);
    let undetected = stats["undetected"].as_i64().unwrap_or(0);
    let total      = malicious + suspicious + harmless + undetected;
    let positives  = malicious + suspicious;
    let vt_url     = format!("https://www.virustotal.com/gui/file/{sha256}");

    if positives > 0 {
        Ok(VtResult::Detected { positives, total, url: vt_url })
    } else {
        Ok(VtResult::Clean { positives, total, url: vt_url })
    }
}

// ─── Router ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/sanitizer", get(sanitizer_page))
        .route("/admin/sanitizer/scan", post(scan))
        .route("/admin/sanitizer/history", get(history))
        .route("/admin/sanitizer/:id", delete(delete_scan))
}
