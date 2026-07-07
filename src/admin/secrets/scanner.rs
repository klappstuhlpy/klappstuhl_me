//! Filesystem scanner — walks configured roots, reads each text file once,
//! runs every rule against each line, and yields `Finding`s.
//!
//! Designed to be cheap to run periodically:
//! * Binary files are detected by a NUL byte in the first 8 KiB and skipped.
//! * Files larger than [`MAX_FILE_BYTES`] are skipped.
//! * Common build / vendor directories are pruned during the walk so we
//!   never even open files inside `.git/`, `node_modules/`, `target/`, etc.
//! * Each line is truncated to [`MAX_LINE_BYTES`] before being run against
//!   the rules to bound regex work on pathological one-line minified files.

use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};
use tracing::warn;

use super::rules::{rules, Severity};

/// Files larger than this are skipped (1 MiB).
const MAX_FILE_BYTES: u64 = 1 * 1024 * 1024;
/// Lines longer than this are truncated for regex matching (8 KiB).
const MAX_LINE_BYTES: usize = 8 * 1024;
/// Bytes read at the start of a file to determine binary-vs-text.
const SNIFF_BYTES: usize = 8 * 1024;

/// Directories we never descend into.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "bower_components",
    "target",
    "build",
    "dist",
    "out",
    ".next",
    ".nuxt",
    "venv",
    ".venv",
    "__pycache__",
    ".cache",
    ".idea",
    ".vscode",
];

/// File extensions we skip outright — opening them just to detect they're
/// binary is wasted I/O.
const SKIP_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff", "mp3", "mp4", "mkv", "webm", "avi", "mov", "wav", "ogg",
    "flac", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "deb", "rpm", "pdf", "doc", "docx", "xls", "xlsx", "ppt",
    "pptx", "exe", "dll", "so", "dylib", "bin", "class", "jar", "wasm", "ttf", "otf", "woff", "woff2", "eot", "db",
    "sqlite", "sqlite3",
];

/// Per-match result.  `finding_hash` dedupes across runs and is also used
/// as the natural key in the `secret_finding` table.
#[derive(Debug, Clone)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub file_path: PathBuf,
    pub line: u32,
    pub snippet: String,
    pub finding_hash: String,
}

/// Counters returned alongside the findings for the scan_run row.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanCounters {
    pub files_scanned: u64,
    pub bytes_scanned: u64,
}

/// Walks all `roots` recursively and returns every matching finding.
///
/// Runs synchronously (file I/O bound) — call from `tokio::task::spawn_blocking`
/// to avoid blocking the async runtime.
pub fn scan(roots: &[PathBuf]) -> (Vec<Finding>, ScanCounters) {
    let mut out = Vec::new();
    let mut counters = ScanCounters::default();
    for root in roots {
        // For roots specifically we surface errors loudly — silent "0
        // files scanned" after a manual click is the worst UX.  Per-file
        // / per-subdir errors below stay silent to avoid spam.
        match std::fs::metadata(root) {
            Ok(m) if m.is_dir() || m.is_file() => walk(root, &mut out, &mut counters),
            Ok(m) => warn!(
                root = %root.display(),
                file_type = ?m.file_type(),
                "secret scan: configured root is neither a file nor a directory, skipping",
            ),
            Err(e) => warn!(
                root = %root.display(),
                error = %e,
                "secret scan: cannot access configured root (does the path exist inside the runtime / does the service user have read access?)",
            ),
        }
    }
    out.sort_by(|a, b| a.file_path.cmp(&b.file_path).then(a.line.cmp(&b.line)));
    (out, counters)
}

fn walk(root: &Path, out: &mut Vec<Finding>, counters: &mut ScanCounters) {
    let metadata = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(_) => return,
    };

    if metadata.is_file() {
        scan_file(root, out, counters);
        return;
    }
    if !metadata.is_dir() {
        return;
    }

    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Skip symlinks to avoid loops / leaving the configured roots.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            let name_s = name.to_string_lossy();
            if SKIP_DIRS.contains(&name_s.as_ref()) || name_s.starts_with('.') && name_s.as_ref() != "." {
                // Skip dotfiles dirs except an explicit root.
                continue;
            }
            walk(&path, out, counters);
        } else if file_type.is_file() {
            scan_file(&path, out, counters);
        }
    }
}

fn scan_file(path: &Path, out: &mut Vec<Finding>, counters: &mut ScanCounters) {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_ascii_lowercase();
        if SKIP_EXTS.contains(&ext_lower.as_str()) {
            return;
        }
    }

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.len() > MAX_FILE_BYTES {
        return;
    }

    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };

    // Sniff for binary contents.
    let mut sniff = vec![0u8; SNIFF_BYTES.min(meta.len() as usize)];
    let read = match file.read(&mut sniff) {
        Ok(n) => n,
        Err(_) => return,
    };
    if sniff[..read].iter().any(|b| *b == 0) {
        return; // binary
    }

    // Reopen as buffered reader (we've consumed `read` bytes from the
    // first handle; cheaper to just open a fresh one).
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let reader = BufReader::new(file);

    counters.files_scanned += 1;
    counters.bytes_scanned += meta.len();

    for (i, line) in reader.lines().enumerate() {
        let Ok(mut line) = line else { continue };
        if line.len() > MAX_LINE_BYTES {
            line.truncate(MAX_LINE_BYTES);
        }
        for rule in rules() {
            if let Some(m) = rule.regex.find(&line) {
                let snippet = redact_snippet(&line, m.start(), m.end());
                let hash = hash_finding(rule.name, path, &snippet);
                out.push(Finding {
                    rule: rule.name,
                    severity: rule.severity,
                    file_path: path.to_path_buf(),
                    line: (i + 1) as u32,
                    snippet,
                    finding_hash: hash,
                });
            }
        }
    }
}

/// Returns a context-trimmed line with the matched span partially redacted
/// (first 4 + last 4 characters kept).  The snippet is what surfaces in
/// the dashboard, so we *don't* want to spill the entire secret to the UI.
fn redact_snippet(line: &str, m_start: usize, m_end: usize) -> String {
    let mut s = String::new();
    let prefix_start = m_start.saturating_sub(20);
    let suffix_end = (m_end + 20).min(line.len());

    s.push_str(&line[prefix_start..m_start]);
    let matched = &line[m_start..m_end];
    if matched.len() <= 8 {
        s.push_str(&"*".repeat(matched.len()));
    } else {
        s.push_str(&matched[..4]);
        s.push_str(&"*".repeat(matched.len().saturating_sub(8)));
        s.push_str(&matched[matched.len() - 4..]);
    }
    s.push_str(&line[m_end..suffix_end]);

    // Trim noisy whitespace so the dashboard line stays compact.
    s.trim().to_string()
}

fn hash_finding(rule: &str, path: &Path, snippet: &str) -> String {
    let mut h = Sha256::new();
    h.update(rule.as_bytes());
    h.update(b"|");
    h.update(path.to_string_lossy().as_bytes());
    h.update(b"|");
    h.update(snippet.as_bytes());
    let digest = h.finalize();
    // Hex-encode; full 64 chars is plenty unique.
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
