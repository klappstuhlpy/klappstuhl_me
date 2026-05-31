//! Multi-sink alert fan-out.
//!
//! All alert payloads in the app share the Discord webhook JSON shape
//! (`{username, embeds: [{title, description, fields, color}]}`), whether they
//! come from the [`crate::discord::Alert`] builder or a hand-built `json!`.
//! That lets us derive a neutral [`AlertNotification`] from any of them and
//! deliver it to non-Discord sinks (ntfy, a generic webhook) without changing
//! the many call sites.

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Serialize)]
pub struct NotificationField {
    pub name: String,
    pub value: String,
}

/// A sink-neutral alert distilled from a Discord-shaped payload.
#[derive(Clone, Serialize)]
pub struct AlertNotification {
    pub title: String,
    /// `"success"`, `"error"`, or `"info"` (mapped from the embed color).
    pub level: String,
    /// Title + description + flattened fields, for plain-text sinks.
    pub body: String,
    pub fields: Vec<NotificationField>,
}

impl AlertNotification {
    /// Extracts a neutral notification from the first embed of a Discord-shaped
    /// payload. Missing pieces degrade to sensible defaults.
    pub fn from_discord_value(value: &Value) -> Self {
        let embed = value.get("embeds").and_then(|e| e.get(0));
        let get_str = |key: &str| -> String {
            embed
                .and_then(|e| e.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        let title = {
            let t = get_str("title");
            if t.is_empty() {
                "Alert".to_string()
            } else {
                t
            }
        };
        let description = get_str("description");
        let color = embed.and_then(|e| e.get("color")).and_then(|c| c.as_u64()).unwrap_or(0);
        // Success greens / error reds used across discord.rs + json! payloads.
        let level = match color {
            0x1c7951 | 0x10b981 => "success",
            0xa4392f | 0xef4444 => "error",
            _ => "info",
        }
        .to_string();

        let fields: Vec<NotificationField> = embed
            .and_then(|e| e.get("fields"))
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|f| {
                        let name = f.get("name")?.as_str()?.to_string();
                        let value = f.get("value")?.as_str()?.to_string();
                        Some(NotificationField { name, value })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut body = description;
        for field in &fields {
            if field.name.is_empty() {
                continue;
            }
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&field.name);
            body.push_str(": ");
            body.push_str(&field.value);
        }

        Self {
            title,
            level,
            body,
            fields,
        }
    }

    /// ASCII-only copy of the title, safe for an HTTP header value (titles can
    /// contain emoji, which are invalid in header values).
    fn ascii_title(&self) -> String {
        let t: String = self
            .title
            .chars()
            .filter(|c| c.is_ascii() && !c.is_ascii_control())
            .collect();
        let t = t.trim().to_string();
        if t.is_empty() {
            "Alert".to_string()
        } else {
            t
        }
    }
}

/// Turns `[label](target)` into plain text. When the label and target are
/// identical (e.g. `[/admin/health](/admin/health)`) only the label is kept;
/// otherwise the target is appended in parentheses so the destination survives.
fn delink(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find('[') {
        // Look for the `](` separating label from target, then the closing `)`.
        if let Some(sep_rel) = rest[open..].find("](") {
            let sep = open + sep_rel;
            if let Some(close_rel) = rest[sep + 2..].find(')') {
                let close = sep + 2 + close_rel;
                let label = &rest[open + 1..sep];
                let target = &rest[sep + 2..close];
                out.push_str(&rest[..open]);
                out.push_str(label);
                if !target.is_empty() && target != label {
                    out.push_str(" (");
                    out.push_str(target);
                    out.push(')');
                }
                rest = &rest[close + 1..];
                continue;
            }
        }
        // Not a well-formed link: emit through the `[` and keep scanning.
        out.push_str(&rest[..=open]);
        rest = &rest[open + 1..];
    }
    out.push_str(rest);
    out
}

/// Strips the Discord-flavoured markdown that appears in alert payloads
/// (`**bold**`, `__underline__`, `~~strike~~`, `` `code` ``, and `[label](url)`
/// links) so plain-text sinks like ntfy don't render the literal markers.
///
/// Single `*` / `_` / `~` are left alone — Discord only uses them paired for
/// emphasis here, and stripping lone ones would mangle identifiers such as
/// `cpu_percent`.
fn strip_markdown(input: &str) -> String {
    let delinked = delink(input);
    // All markers are ASCII, so a byte scan can't split a multi-byte char.
    let b = delinked.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'`' {
            i += 1;
            continue;
        }
        if (c == b'*' || c == b'_' || c == b'~') && i + 1 < b.len() && b[i + 1] == c {
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8(out).unwrap_or(delinked)
}

/// POSTs the notification to an ntfy topic URL as a plain-text push.
pub async fn send_ntfy(client: &reqwest::Client, url: &str, note: &AlertNotification) {
    let priority = match note.level.as_str() {
        "error" => "high",
        "success" => "default",
        _ => "low",
    };
    let tags = match note.level.as_str() {
        "error" => "rotating_light",
        "success" => "white_check_mark",
        _ => "information_source",
    };
    // ntfy renders plain text, so strip the Discord markdown from the body and
    // title (the title header is also ASCII-folded for header-value safety).
    let body = if note.body.is_empty() {
        strip_markdown(&note.title)
    } else {
        strip_markdown(&note.body)
    };
    let _ = client
        .post(url)
        .header("Title", strip_markdown(&note.ascii_title()))
        .header("Priority", priority)
        .header("Tags", tags)
        .body(body)
        .send()
        .await;
}

/// POSTs the neutral notification as JSON to a generic webhook URL.
pub async fn send_webhook(client: &reqwest::Client, url: &str, note: &AlertNotification) {
    let _ = client.post(url).json(note).send().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_neutral_notification_from_discord_payload() {
        let payload = json!({
            "username": "klappstuhl",
            "embeds": [{
                "title": "🔴 web is down",
                "description": "the site is unreachable",
                "color": 0xef4444u32,
                "fields": [
                    { "name": "Target", "value": "web", "inline": false },
                    { "name": "Error", "value": "timeout", "inline": false }
                ]
            }]
        });
        let note = AlertNotification::from_discord_value(&payload);
        assert_eq!(note.title, "🔴 web is down");
        assert_eq!(note.level, "error");
        assert_eq!(note.fields.len(), 2);
        assert!(note.body.contains("the site is unreachable"));
        assert!(note.body.contains("Target: web"));
        // Header-safe title strips the emoji.
        assert_eq!(note.ascii_title(), "web is down");
    }

    #[test]
    fn defaults_when_fields_absent() {
        let note = AlertNotification::from_discord_value(&json!({}));
        assert_eq!(note.title, "Alert");
        assert_eq!(note.level, "info");
        assert!(note.fields.is_empty());
    }

    #[test]
    fn strips_discord_markdown_for_ntfy() {
        let input = "**Target:** `web`\n**Kind:** http\n\nCheck the [/admin/health](/admin/health) dashboard.";
        let out = strip_markdown(input);
        assert_eq!(
            out,
            "Target: web\nKind: http\n\nCheck the /admin/health dashboard."
        );
    }

    #[test]
    fn delink_keeps_distinct_targets() {
        assert_eq!(delink("see [the docs](https://x.test)"), "see the docs (https://x.test)");
        // Identical label/target collapses to a single copy.
        assert_eq!(delink("[/admin/health](/admin/health)"), "/admin/health");
    }

    #[test]
    fn strip_leaves_single_markers_and_other_text() {
        // Lone underscores in identifiers must survive.
        assert_eq!(strip_markdown("cpu_percent over threshold"), "cpu_percent over threshold");
        // Strikethrough + underline pairs are removed.
        assert_eq!(strip_markdown("~~old~~ __new__"), "old new");
    }

    #[test]
    fn strip_handles_multibyte_text() {
        // Emoji / non-ASCII bytes must not be corrupted by the byte scan.
        assert_eq!(strip_markdown("🔴 **web** is down"), "🔴 web is down");
    }
}
