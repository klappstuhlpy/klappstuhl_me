//! Cloudflare Tunnel (cfd_tunnel) + DNS REST client.
//!
//! For a *remotely-managed* tunnel (created in the dashboard, run with
//! `cloudflared tunnel run --token …`) there is no local `config.yml` or
//! credentials file — the public-hostname ingress lives in Cloudflare and is
//! edited via the API. This client reads and writes that ingress so
//! `/admin/proxy` can import existing routes and push new ones, and upserts the
//! matching proxied `CNAME → <tunnel>.cfargotunnel.com` DNS records so new
//! hostnames actually resolve.
//!
//! Pushing a config *replaces* the tunnel's entire ingress list, so the caller
//! must import first (DB becomes the source of truth) before pushing.

use anyhow::Context;
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Suffix every tunnel hostname's CNAME points at.
fn tunnel_cname_target(tunnel_id: &str) -> String {
    format!("{tunnel_id}.cfargotunnel.com")
}

#[derive(Clone)]
pub struct CfTunnel {
    client: reqwest::Client,
    api_token: String,
    account_id: String,
    tunnel_id: String,
    /// Zone used for DNS upserts. DNS management is skipped when `None`.
    zone_id: Option<String>,
}

/// One ingress rule in the Cloudflare Tunnel configuration. The trailing
/// catch-all has no `hostname` (just `service: http_status:404`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IngressRule {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hostname: Option<String>,
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default, rename = "originRequest")]
    pub origin_request: Option<serde_json::Value>,
}

impl IngressRule {
    /// `true` for the catch-all rule (no hostname).
    pub fn is_catch_all(&self) -> bool {
        self.hostname.as_deref().map(str::is_empty).unwrap_or(true)
    }
}

impl CfTunnel {
    pub fn new(
        client: reqwest::Client,
        api_token: String,
        account_id: String,
        tunnel_id: String,
        zone_id: Option<String>,
    ) -> Self {
        Self {
            client,
            api_token,
            account_id,
            tunnel_id,
            zone_id,
        }
    }

    fn config_url(&self) -> String {
        format!(
            "{API_BASE}/accounts/{}/cfd_tunnel/{}/configurations",
            self.account_id, self.tunnel_id
        )
    }

    /// GET the tunnel's current ingress configuration.
    pub async fn get_ingress(&self) -> anyhow::Result<Vec<IngressRule>> {
        let url = self.config_url();
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .context("GET tunnel configuration")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let parsed: CfResponse<TunnelConfigResult> =
            serde_json::from_str(&text).with_context(|| format!("parse tunnel config response ({status}): {text}"))?;
        if !parsed.success {
            anyhow::bail!("Cloudflare API error ({status}) reading tunnel config: {}", parsed.errors_str());
        }
        Ok(parsed
            .result
            .and_then(|r| r.config)
            .map(|c| c.ingress)
            .unwrap_or_default())
    }

    /// PUT a full ingress configuration (replaces the tunnel's existing one).
    pub async fn put_ingress(&self, ingress: &[IngressRule]) -> anyhow::Result<()> {
        let url = self.config_url();
        let body = serde_json::json!({ "config": { "ingress": ingress } });
        let resp = self
            .client
            .put(&url)
            .bearer_auth(&self.api_token)
            .json(&body)
            .send()
            .await
            .context("PUT tunnel configuration")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let parsed: CfResponse<serde_json::Value> =
            serde_json::from_str(&text).with_context(|| format!("parse PUT response ({status}): {text}"))?;
        if !parsed.success {
            anyhow::bail!("Cloudflare API error ({status}) writing tunnel config: {}", parsed.errors_str());
        }
        Ok(())
    }

    /// Ensure a proxied `CNAME <hostname> → <tunnel>.cfargotunnel.com` exists,
    /// creating or updating it. No-op (with an error) when no zone is set.
    pub async fn upsert_dns(&self, hostname: &str) -> anyhow::Result<()> {
        let zone = self
            .zone_id
            .as_deref()
            .context("cloudflare_zone_id not set — cannot manage DNS for tunnel hostnames")?;
        let target = tunnel_cname_target(&self.tunnel_id);
        let record = serde_json::json!({
            "type": "CNAME",
            "name": hostname,
            "content": target,
            "proxied": true,
            "comment": "Managed by klappstuhl.me",
        });

        // Look for an existing record for this exact name.
        let list_url = format!("{API_BASE}/zones/{zone}/dns_records?type=CNAME&name={hostname}");
        let listed: CfResponse<Vec<DnsRecord>> = self
            .client
            .get(&list_url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .context("list DNS records")?
            .json()
            .await
            .context("parse DNS list")?;
        if !listed.success {
            anyhow::bail!("Cloudflare API error listing DNS for {hostname}: {}", listed.errors_str());
        }

        let existing = listed.result.unwrap_or_default().into_iter().next();
        let (method_url, is_update) = match &existing {
            Some(rec) => (format!("{API_BASE}/zones/{zone}/dns_records/{}", rec.id), true),
            None => (format!("{API_BASE}/zones/{zone}/dns_records"), false),
        };
        let req = if is_update {
            self.client.put(&method_url)
        } else {
            self.client.post(&method_url)
        };
        let resp = req
            .bearer_auth(&self.api_token)
            .json(&record)
            .send()
            .await
            .context("upsert DNS record")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let parsed: CfResponse<serde_json::Value> =
            serde_json::from_str(&text).with_context(|| format!("parse DNS upsert ({status}): {text}"))?;
        if !parsed.success {
            anyhow::bail!("Cloudflare API error upserting DNS for {hostname}: {}", parsed.errors_str());
        }
        Ok(())
    }
}

// ─── Cloudflare REST envelope ───────────────────────────────────────────────

#[derive(Deserialize)]
struct CfResponse<T> {
    success: bool,
    #[serde(default)]
    errors: Vec<CfError>,
    // `Option<T>` is already treated as optional by serde (None when missing);
    // adding `#[serde(default)]` here would wrongly require `T: Default`.
    result: Option<T>,
}

impl<T> CfResponse<T> {
    fn errors_str(&self) -> String {
        if self.errors.is_empty() {
            "no error detail".to_string()
        } else {
            self.errors
                .iter()
                .map(|e| format!("[{}] {}", e.code, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        }
    }
}

#[derive(Deserialize)]
struct CfError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct TunnelConfigResult {
    #[serde(default)]
    config: Option<IngressConfig>,
}

#[derive(Deserialize)]
struct IngressConfig {
    #[serde(default)]
    ingress: Vec<IngressRule>,
}

#[derive(Deserialize, Default)]
struct DnsRecord {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_all_detection() {
        let catch = IngressRule {
            service: "http_status:404".into(),
            ..Default::default()
        };
        assert!(catch.is_catch_all());
        let real = IngressRule {
            hostname: Some("a.example.com".into()),
            service: "http://localhost:80".into(),
            ..Default::default()
        };
        assert!(!real.is_catch_all());
    }

    #[test]
    fn cname_target_format() {
        assert_eq!(tunnel_cname_target("abc-123"), "abc-123.cfargotunnel.com");
    }

    #[test]
    fn ingress_rule_round_trips_without_nulls() {
        let rule = IngressRule {
            hostname: Some("a.example.com".into()),
            service: "https://10.0.0.1:8443".into(),
            path: None,
            origin_request: Some(serde_json::json!({ "noTLSVerify": true })),
        };
        let json = serde_json::to_string(&rule).unwrap();
        // Optional empty fields must be omitted (Cloudflare rejects nulls).
        assert!(!json.contains("\"path\""));
        assert!(json.contains("originRequest"));
        assert!(json.contains("a.example.com"));
    }
}
