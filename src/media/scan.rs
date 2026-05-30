//! Reusable file-scanning backend shared by the admin sanitizer page and the
//! public `/api/scan` endpoint.
//!
//! Talks to a ClamAV daemon (`clamav_addr`) over the INSTREAM protocol and
//! looks up the file's SHA-256 on VirusTotal (`virustotal_api_key`). Both are
//! optional: when a backend isn't configured the corresponding fields stay
//! `None`, so a caller can still record/return whatever the other backend
//! produced (or nothing, if neither is set).

use std::fmt::Write as _;
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use utoipa::ToSchema;

use crate::AppState;

/// The outcome of scanning a single blob of bytes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanReport {
    /// Lowercase hex SHA-256 of the scanned bytes.
    pub sha256: String,
    /// Size of the scanned file in bytes.
    pub file_size: i64,
    /// `Some(true)` if ClamAV reported the file clean, `Some(false)` if a
    /// signature matched, `None` if ClamAV is not configured or errored.
    pub clamav_clean: Option<bool>,
    /// The signature name when ClamAV found something (or an error string).
    pub clamav_virus: Option<String>,
    /// VirusTotal verdict: `"clean"`, `"detected"`, `"unknown"`, `"error"`, or
    /// `None` when VirusTotal is not configured.
    pub vt_status: Option<String>,
    /// Number of engines that flagged the file on VirusTotal.
    pub vt_positives: Option<i64>,
    /// Total number of engines that analysed the file on VirusTotal.
    pub vt_total: Option<i64>,
    /// Link to the VirusTotal report for this file.
    pub vt_url: Option<String>,
    /// Coarse overall verdict derived from both backends: `"infected"`,
    /// `"clean"`, or `"unknown"`.
    pub verdict: String,
}

/// Computes the lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

/// Runs the configured scanners (ClamAV + VirusTotal) over `data` and returns
/// a combined report. Each backend is bounded by its own timeout and degrades
/// to `None`/`"error"` on failure rather than propagating an error, so the
/// caller always gets a usable report.
pub async fn scan_bytes(state: &AppState, data: &[u8]) -> ScanReport {
    let file_size = data.len() as i64;
    let sha256 = sha256_hex(data);

    // ── ClamAV ───────────────────────────────────────────────────────────────
    let (clamav_clean, clamav_virus) = if let Some(addr) = state.config().clamav_addr.as_deref() {
        match tokio::time::timeout(Duration::from_secs(60), clamav_scan(addr, data)).await {
            Ok(Ok(ClamResult::Clean)) => (Some(true), None),
            Ok(Ok(ClamResult::Found(v))) => (Some(false), Some(v)),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "ClamAV scan error");
                (None, Some(format!("scan error: {e}")))
            }
            Err(_) => (None, Some("scan timed out".into())),
        }
    } else {
        (None, None)
    };

    // ── VirusTotal ─────────────────────────────────────────────────────────────
    let (vt_status, vt_positives, vt_total, vt_url) = if let Some(key) = state.config().virustotal_api_key.as_deref() {
        match tokio::time::timeout(Duration::from_secs(30), vt_lookup(&state.client, key, &sha256)).await {
            Ok(Ok(VtResult::Clean { positives, total, url })) => {
                (Some("clean".to_owned()), Some(positives), Some(total), Some(url))
            }
            Ok(Ok(VtResult::Detected { positives, total, url })) => {
                (Some("detected".to_owned()), Some(positives), Some(total), Some(url))
            }
            Ok(Ok(VtResult::Unknown)) => (Some("unknown".to_owned()), None, None, None),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "VirusTotal lookup error");
                (Some("error".to_owned()), None, None, None)
            }
            Err(_) => (Some("error".to_owned()), None, None, None),
        }
    } else {
        (None, None, None, None)
    };

    // Coarse overall verdict so callers (audit log, API clients) have a single
    // field to branch on, with the per-backend detail underneath.
    let verdict = if clamav_clean == Some(false) || vt_status.as_deref() == Some("detected") {
        "infected"
    } else if clamav_clean == Some(true) || vt_status.as_deref() == Some("clean") {
        "clean"
    } else {
        "unknown"
    }
    .to_owned();

    ScanReport {
        sha256,
        file_size,
        clamav_clean,
        clamav_virus,
        vt_status,
        vt_positives,
        vt_total,
        vt_url,
        verdict,
    }
}

// ─── ClamAV client ──────────────────────────────────────────────────────────

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
    Clean { positives: i64, total: i64, url: String },
    Detected { positives: i64, total: i64, url: String },
    Unknown,
}

async fn vt_lookup(client: &reqwest::Client, api_key: &str, sha256: &str) -> anyhow::Result<VtResult> {
    let url = format!("https://www.virustotal.com/api/v3/files/{sha256}");
    let resp = client.get(&url).header("x-apikey", api_key).send().await?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(VtResult::Unknown);
    }
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("VT API returned {}", resp.status()));
    }

    let json: serde_json::Value = resp.json().await?;
    let stats = &json["data"]["attributes"]["last_analysis_stats"];
    let malicious = stats["malicious"].as_i64().unwrap_or(0);
    let suspicious = stats["suspicious"].as_i64().unwrap_or(0);
    let harmless = stats["harmless"].as_i64().unwrap_or(0);
    let undetected = stats["undetected"].as_i64().unwrap_or(0);
    let total = malicious + suspicious + harmless + undetected;
    let positives = malicious + suspicious;
    let vt_url = format!("https://www.virustotal.com/gui/file/{sha256}");

    if positives > 0 {
        Ok(VtResult::Detected {
            positives,
            total,
            url: vt_url,
        })
    } else {
        Ok(VtResult::Clean {
            positives,
            total,
            url: vt_url,
        })
    }
}
