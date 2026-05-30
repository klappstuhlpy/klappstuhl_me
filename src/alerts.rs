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
            if t.is_empty() { "Alert".to_string() } else { t }
        };
        let description = get_str("description");
        let color = embed
            .and_then(|e| e.get("color"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0);
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

        Self { title, level, body, fields }
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
        if t.is_empty() { "Alert".to_string() } else { t }
    }
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
    let body = if note.body.is_empty() { note.title.clone() } else { note.body.clone() };
    let _ = client
        .post(url)
        .header("Title", note.ascii_title())
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
}
