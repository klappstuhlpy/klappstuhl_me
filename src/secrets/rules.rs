//! Detection rules for the secret scanner.
//!
//! Each rule is a (name, severity, regex) triple compiled once on first use.
//! Patterns are tuned for *low false-positive rate over high coverage* —
//! we'd rather miss a generic API key than spam the dashboard with hits on
//! base64-encoded test fixtures.  Generic high-entropy detection is
//! deliberately omitted for that reason; the rules below all anchor on a
//! provider-specific prefix or unique structural marker.

use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Confirmed live-credential format (AWS keys, GitHub PATs, etc.) —
    /// finding a match here almost always means a real leak.
    Critical,
    /// Likely-secret format (Stripe test keys, JWTs).  Worth investigating
    /// but might be a test fixture.
    High,
    /// Possible secret — broader patterns that catch more but false-positive
    /// occasionally (private-key headers in documentation, for example).
    Medium,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
        }
    }
}

#[derive(Debug)]
pub struct Rule {
    pub name: &'static str,
    pub severity: Severity,
    pub regex: Regex,
}

/// Returns the compiled rule set (lazy, one-shot).
pub fn rules() -> &'static [Rule] {
    static SLOT: OnceLock<Vec<Rule>> = OnceLock::new();
    SLOT.get_or_init(build_rules).as_slice()
}

fn build_rules() -> Vec<Rule> {
    let mk = |name: &'static str, severity: Severity, pat: &str| -> Rule {
        Rule {
            name,
            severity,
            regex: Regex::new(pat).expect("invalid secret rule regex"),
        }
    };

    vec![
        // ── Cloud providers ──────────────────────────────────────────
        mk("AWS Access Key",      Severity::Critical, r"AKIA[0-9A-Z]{16}"),
        mk("AWS Secret Key",      Severity::Critical,
            r#"(?i)aws[_-]?secret[_-]?access[_-]?key["'\s:=]+[A-Za-z0-9/+=]{40}"#),
        mk("Google API Key",      Severity::Critical, r"AIza[0-9A-Za-z\-_]{35}"),
        mk("GCP Service Account", Severity::Critical, r#""type"\s*:\s*"service_account""#),

        // ── Source / DevOps ──────────────────────────────────────────
        mk("GitHub Token",        Severity::Critical, r"gh[pousr]_[A-Za-z0-9]{36,255}"),
        mk("GitLab PAT",          Severity::Critical, r"glpat-[A-Za-z0-9\-_]{20,}"),
        mk("npm Token",           Severity::Critical, r"npm_[A-Za-z0-9]{36}"),

        // ── Payments / SaaS ──────────────────────────────────────────
        mk("Stripe Live Secret",  Severity::Critical, r"sk_live_[0-9a-zA-Z]{24,}"),
        mk("Stripe Test Secret",  Severity::High,     r"sk_test_[0-9a-zA-Z]{24,}"),
        mk("Slack Token",         Severity::Critical, r"xox[baprs]-[A-Za-z0-9-]{10,}"),
        mk("Slack Webhook",       Severity::High,
            r"https://hooks\.slack\.com/services/T[A-Z0-9]+/B[A-Z0-9]+/[A-Za-z0-9]{24}"),
        mk("Discord Bot Token",   Severity::Critical,
            r"[MN][A-Za-z\d]{23}\.[\w-]{6}\.[\w-]{27,}"),
        mk("Discord Webhook URL", Severity::High,
            r"https://(?:canary\.|ptb\.)?discord(?:app)?\.com/api/webhooks/\d+/[A-Za-z0-9\-_]+"),

        // ── AI providers ─────────────────────────────────────────────
        mk("OpenAI API Key",      Severity::Critical, r"sk-(?:proj-)?[A-Za-z0-9_-]{40,}"),
        mk("Anthropic API Key",   Severity::Critical, r"sk-ant-[a-z0-9]+-[A-Za-z0-9_-]{32,}"),

        // ── Keys / certificates / tokens ─────────────────────────────
        mk("Private Key Block",   Severity::Critical,
            r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED |PGP )?PRIVATE KEY-----"),
        mk("JWT",                 Severity::Medium,
            r"eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"),

        // ── Generic high-confidence env patterns ─────────────────────
        // password=plaintext or DATABASE_URL with embedded creds.
        mk("Database URL with password", Severity::High,
            r"(?:postgres|postgresql|mysql|mongodb|redis|amqp)://[^:\s]+:[^@\s]+@[^/\s]+"),
    ]
}
