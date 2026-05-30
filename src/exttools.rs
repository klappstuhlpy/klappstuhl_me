//! Thin wrappers around optional external binaries (Chromium, ffmpeg) used by
//! the render/convert API endpoints.
//!
//! Everything here is best-effort and config-gated: if the binary isn't
//! configured or found on `PATH`, the caller surfaces a `503`. The actual
//! command-line flags target reasonably current Chromium/ffmpeg builds and may
//! need tuning per environment — these endpoints are dormant until an operator
//! installs and (optionally) points config at the tools.

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use crate::AppState;

/// A scratch file in the system temp dir, removed on drop.
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    pub fn new(ext: &str) -> Self {
        let name = format!("klappstuhl-{}.{ext}", nanoid::nanoid!(12));
        TempFile { path: std::env::temp_dir().join(name) }
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub fn to_arg(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Returns true if a bare command name resolves on `PATH` (or is an existing
/// absolute path).
fn is_runnable(cmd: &str) -> bool {
    let p = std::path::Path::new(cmd);
    if p.is_absolute() {
        return p.is_file();
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path_var).any(|dir| {
        exts.iter().any(|ext| {
            let candidate = dir.join(format!("{cmd}{ext}"));
            candidate.is_file()
        })
    })
}

/// Resolves the Chromium/Chrome binary: configured path first, then common
/// names on `PATH`.
pub fn chromium(state: &AppState) -> Option<String> {
    if let Some(p) = state.config().chromium_path.clone() {
        return is_runnable(&p).then_some(p);
    }
    ["chromium", "chromium-browser", "google-chrome", "chrome", "chrome.exe"]
        .into_iter()
        .find(|c| is_runnable(c))
        .map(String::from)
}

/// Resolves the ffmpeg binary: configured path first, then `ffmpeg` on `PATH`.
pub fn ffmpeg(state: &AppState) -> Option<String> {
    if let Some(p) = state.config().ffmpeg_path.clone() {
        return is_runnable(&p).then_some(p);
    }
    is_runnable("ffmpeg").then(|| "ffmpeg".to_string())
}

/// Options controlling a screenshot capture.
pub struct ShotOptions {
    pub width: u32,
    pub height: u32,
    pub dark_mode: bool,
    pub mobile: bool,
    pub full_page: bool,
}

/// Captures a PNG screenshot of `url` with headless Chromium.
pub async fn screenshot(bin: &str, url: &str, opts: &ShotOptions) -> Result<Vec<u8>, String> {
    let out = TempFile::new("png");
    // Full-page is approximated with a tall viewport; new headless captures
    // the viewport rather than the whole scroll height.
    let height = if opts.full_page { opts.height.max(3000) } else { opts.height };
    let window = format!("--window-size={},{}", opts.width.max(1), height.max(1));
    let screenshot_arg = format!("--screenshot={}", out.to_arg());

    let mut args: Vec<String> = vec![
        "--headless=new".into(),
        "--disable-gpu".into(),
        "--no-sandbox".into(),
        "--hide-scrollbars".into(),
        "--no-first-run".into(),
        window,
        screenshot_arg,
    ];
    if opts.dark_mode {
        args.push("--force-dark-mode".into());
        args.push("--enable-features=WebContentsForceDark".into());
    }
    if opts.mobile {
        args.push("--user-agent=Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Mobile/15E148 Safari/604.1".into());
    }
    args.push(url.to_string());

    run(bin, &args, Duration::from_secs(30)).await?;
    tokio::fs::read(out.path()).await.map_err(|e| format!("screenshot not produced: {e}"))
}

/// Renders an HTML string to PDF bytes with headless Chromium.
pub async fn html_to_pdf(bin: &str, html: &str) -> Result<Vec<u8>, String> {
    let input = TempFile::new("html");
    tokio::fs::write(input.path(), html).await.map_err(|e| e.to_string())?;
    let out = TempFile::new("pdf");
    let args = vec![
        "--headless=new".to_string(),
        "--disable-gpu".to_string(),
        "--no-sandbox".to_string(),
        "--no-pdf-header-footer".to_string(),
        format!("--print-to-pdf={}", out.to_arg()),
        format!("file://{}", input.to_arg()),
    ];
    run(bin, &args, Duration::from_secs(30)).await?;
    tokio::fs::read(out.path()).await.map_err(|e| format!("pdf not produced: {e}"))
}

/// Runs ffmpeg to transcode `input` (written with extension `in_ext`) using the
/// given output args, returning the produced bytes.
pub async fn ffmpeg_convert(
    bin: &str,
    input: &[u8],
    in_ext: &str,
    out_ext: &str,
    out_args: &[&str],
) -> Result<Vec<u8>, String> {
    let infile = TempFile::new(in_ext);
    tokio::fs::write(infile.path(), input).await.map_err(|e| e.to_string())?;
    let outfile = TempFile::new(out_ext);

    let mut args: Vec<String> = vec!["-y".into(), "-i".into(), infile.to_arg()];
    args.extend(out_args.iter().map(|s| s.to_string()));
    args.push(outfile.to_arg());

    run(bin, &args, Duration::from_secs(120)).await?;
    tokio::fs::read(outfile.path()).await.map_err(|e| format!("conversion produced no output: {e}"))
}

/// Spawns `bin` with `args`, enforcing a timeout. Returns an error string on
/// non-zero exit or timeout (stderr tail included).
async fn run(bin: &str, args: &[String], timeout: Duration) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| format!("could not start {bin}: {e}"))?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("{bin} failed: {e}")),
        Err(_) => return Err(format!("{bin} timed out")),
    };
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        Err(format!("{bin} exited with {}: {tail}", output.status))
    }
}

// ─── SSRF guard for the screenshot URL ───────────────────────────────────────

/// Returns true for loopback/private/reserved addresses an SSRF attacker might
/// target.
pub fn ip_is_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().map(|v4| ip_is_blocked(&IpAddr::V4(v4))).unwrap_or(false)
        }
    }
}

/// Validates a user-supplied URL for server-side fetching/rendering: http(s)
/// only, host not localhost, and every resolved address public.
pub async fn assert_public_url(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid url".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("url must use http or https".into());
    }
    let host = url.host_str().ok_or("url has no host")?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err("refusing to render a local address".into());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let mut resolved = false;
    for addr in tokio::net::lookup_host((host, port)).await.map_err(|_| "could not resolve url host".to_string())? {
        resolved = true;
        if ip_is_blocked(&addr.ip()) {
            return Err("refusing to render a private or reserved address".into());
        }
    }
    if !resolved {
        return Err("could not resolve url host".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_allows_public() {
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.0.1", "169.254.0.1", "::1", "fe80::1"] {
            assert!(ip_is_blocked(&ip.parse().unwrap()), "{ip} should block");
        }
        for ip in ["8.8.8.8", "1.1.1.1"] {
            assert!(!ip_is_blocked(&ip.parse().unwrap()), "{ip} should allow");
        }
    }
}
