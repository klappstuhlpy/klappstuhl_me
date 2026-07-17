use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::OnceLock,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::key::SecretKey;
use crate::{cli::PROGRAM_NAME, discord::Webhook};

/// Where the site delivers its own alerts.
///
/// This was a fan-out across Discord / ntfy / a generic webhook / SMTP, driven by
/// the admin control plane's metric, health, secret and backup alerts. Those left
/// with Vantage (which has its own sinks), and the only caller remaining here is
/// [`crate::AppState::send_alert`], which posts to Discord. The other three sinks
/// were removed rather than left as config an operator could set and never see
/// fire.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AlertsConfig {
    /// Discord incoming-webhook URL.
    #[serde(default)]
    pub discord_webhook_url: Option<Webhook>,
}

/// Discord OAuth2 settings for identity linking (login with Discord, link
/// Discord account to an existing user). Disabled unless all three fields are
/// set. Register an application at <https://discord.com/developers/applications>
/// and add the redirect URI to the OAuth2 settings there.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DiscordConfig {
    /// OAuth2 application client ID.
    #[serde(default)]
    pub client_id: Option<String>,
    /// OAuth2 application client secret.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// The full callback URL registered in the Discord developer portal,
    /// e.g. `"https://klappstuhl.me/auth/discord/callback"`.
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

impl DiscordConfig {
    /// Whether Discord OAuth is fully configured and available.
    pub fn enabled(&self) -> bool {
        self.client_id.as_deref().is_some_and(|s| !s.is_empty())
            && self.client_secret.as_deref().is_some_and(|s| !s.is_empty())
            && self.redirect_uri.as_deref().is_some_and(|s| !s.is_empty())
    }
}

/// Pastebin settings (`/paste`, `/pastes`, `/p/<id>`, `POST /p`).
///
/// Every field has a default, so an install that never writes a `paste` block
/// gets the shipped behaviour: anonymous pastes on, 512 KB for accounts,
/// 256 KB and a forced 30-day TTL for anonymous ones.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PasteConfig {
    /// Whether strangers may create pastes without an account (the `curl`
    /// endpoint, `POST /p`). This is the only surface on the site that
    /// unauthenticated visitors can write to — turn it off if it gets abused.
    #[serde(default = "default_true")]
    pub anonymous: bool,
    /// Maximum body size for a signed-in account's paste, in bytes.
    #[serde(default = "default_paste_max_bytes")]
    pub max_bytes: usize,
    /// Maximum body size for an anonymous paste, in bytes.
    #[serde(default = "default_paste_anon_max_bytes")]
    pub anonymous_max_bytes: usize,
    /// Forced TTL for anonymous pastes, in days — nothing anonymous lives
    /// forever. A shorter requested expiry is honoured; a longer one is capped.
    #[serde(default = "default_paste_anon_ttl_days")]
    pub anonymous_ttl_days: i64,
    /// Maximum pastes a non-admin account may own. Admins are unlimited.
    #[serde(default = "default_paste_account_limit")]
    pub account_limit: usize,
    /// Maximum total paste bytes a non-admin account may store.
    #[serde(default = "default_paste_account_max_total_bytes")]
    pub account_max_total_bytes: i64,
    /// The syntect theme the viewer highlights with by default. Visitors can
    /// override it per-browser from the viewer's theme picker.
    #[serde(default = "default_paste_theme")]
    pub default_theme: String,
}

fn default_true() -> bool {
    true
}

fn default_paste_max_bytes() -> usize {
    512 * 1024
}

fn default_paste_anon_max_bytes() -> usize {
    256 * 1024
}

fn default_paste_anon_ttl_days() -> i64 {
    30
}

fn default_paste_account_limit() -> usize {
    100
}

fn default_paste_account_max_total_bytes() -> i64 {
    16 * 1024 * 1024
}

fn default_paste_theme() -> String {
    "base16-ocean.dark".to_string()
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            anonymous: default_true(),
            max_bytes: default_paste_max_bytes(),
            anonymous_max_bytes: default_paste_anon_max_bytes(),
            anonymous_ttl_days: default_paste_anon_ttl_days(),
            account_limit: default_paste_account_limit(),
            account_max_total_bytes: default_paste_account_max_total_bytes(),
            default_theme: default_paste_theme(),
        }
    }
}

/// The server configuration.
///
/// Field/declaration order is the canonical on-disk order: `load()` rewrites
/// `config.json` to match it (and the grouped sub-maps), so a hand-edited file
/// is normalised on the next start-up.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Whether the server is running a production build or not
    #[serde(default)]
    pub production: bool,
    /// The domains that are registered to this server.
    ///
    /// These must *not* have any schemes.
    #[serde(default)]
    pub domains: Vec<String>,
    /// The server IP and port configuration
    #[serde(default)]
    pub server: ServerConfig,
    /// The secret key used for all crypto related functionality in the server.
    ///
    /// Microbenching makes it evident that cloning this without an Arc is around ~4x faster.
    pub secret_key: SecretKey,
    /// Where the site posts its own alerts (Discord only — see [`AlertsConfig`]).
    #[serde(default)]
    pub alerts: AlertsConfig,
    /// ClamAV daemon address (e.g. `"127.0.0.1:3310"`).
    /// When set, uploaded images are scanned by clamd over INSTREAM. The admin
    /// *sanitizer page* moved to Vantage; upload scanning stayed here, which is
    /// why this key did too.
    #[serde(default)]
    pub clamav_addr: Option<String>,
    /// VirusTotal public API key.
    /// When set, an uploaded file's SHA-256 is looked up on VirusTotal.
    #[serde(default)]
    pub virustotal_api_key: Option<String>,
    /// Path to a Chromium/Chrome binary for the screenshot and Markdown→PDF
    /// render endpoints. When unset, common names on `PATH` are tried; if none
    /// is found those endpoints return 503.
    #[serde(default)]
    pub chromium_path: Option<String>,
    /// Path to an `ffmpeg` binary for the video/HEIC conversion endpoints.
    /// When unset, `ffmpeg` on `PATH` is used; otherwise those endpoints
    /// return 503.
    #[serde(default)]
    pub ffmpeg_path: Option<String>,
    /// Maximum accepted size of a single uploaded image, in bytes. `0`/unset
    /// defaults to 10 MiB. The upload handler streams each field and aborts as
    /// soon as this is exceeded, so an oversized (or maliciously huge) upload
    /// can never be buffered whole in memory.
    #[serde(default)]
    pub max_upload_bytes: Option<u64>,
    /// Pastebin limits and the anonymous-paste switch.
    #[serde(default)]
    pub paste: PasteConfig,
    /// Discord OAuth2 settings for identity linking (bot dashboard access).
    /// Off unless all three fields (`client_id`, `client_secret`, `redirect_uri`) are set.
    #[serde(default)]
    pub discord: DiscordConfig,
    /// Optional shared key for the cross-app SSO handoff to the Percy dashboard.
    /// Set to the **same** value as the dashboard's `sso_secret` to make the
    /// `/percy` link log a linked-Discord user straight into the dashboard.
    /// Unset → `/percy` is a plain redirect (the dashboard does its own login).
    #[serde(default)]
    pub sso_secret: Option<SecretKey>,
    /// Shared service token that authorises the bot (Percy) to provision
    /// per-guild image-gallery keys via `POST /api/v1/guilds/:id/provision-key`.
    /// Set to the same high-entropy value as Percy's `KLAPPSTUHL_ME_PROVISION_TOKEN`.
    /// This is a narrow, single-purpose credential — its only power is
    /// get-or-create of a guild's `images:guild` key — so the bot never needs a
    /// personal/all-access API key. Unset → the provision endpoint returns 503
    /// (the feature is off).
    #[serde(default)]
    pub gallery_provision_token: Option<String>,
}

/// Default per-file upload ceiling (10 MiB) used when `max_upload_bytes` is
/// unset or zero.
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;

impl Config {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            production: false,
            domains: Vec::new(),
            server: ServerConfig::default(),
            secret_key: SecretKey::random()?,
            alerts: AlertsConfig::default(),
            clamav_addr: None,
            virustotal_api_key: None,
            chromium_path: None,
            ffmpeg_path: None,
            max_upload_bytes: None,
            paste: PasteConfig::default(),
            discord: DiscordConfig::default(),
            sso_secret: None,
            gallery_provision_token: None,
        })
    }

    /// Effective per-file upload size limit in bytes, applying the
    /// [`DEFAULT_MAX_UPLOAD_BYTES`] fallback when unset or zero.
    pub fn effective_max_upload_bytes(&self) -> u64 {
        self.max_upload_bytes
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES)
    }

    pub fn path() -> anyhow::Result<PathBuf> {
        let mut path = dirs::config_dir().context("could not find a config directory for the current user")?;
        path.push(PROGRAM_NAME);
        path.push("config.json");
        Ok(path)
    }

    pub fn load() -> anyhow::Result<Self> {
        let path = Self::path()?;
        if path.exists() {
            let text = std::fs::read_to_string(&path).context("could not read config file")?;
            // Migrate any legacy flat keys (`cloudflare_api_token`, `proxy_kind`,
            // …) into their grouped sub-maps before deserialising, so existing
            // configs keep working after the regrouping.
            let mut value: serde_json::Value = serde_json::from_str(&text).context("could not parse config file")?;
            migrate_flat_to_grouped(&mut value);
            let config: Config = serde_json::from_value(value).context("could not parse config file")?;

            // Keep the on-disk file normalised to the canonical key order + the
            // grouped layout. Rewrite only when it actually differs, so a
            // tidy file doesn't churn its mtime every start.
            if let Ok(canonical) = serde_json::to_string_pretty(&config) {
                if canonical != text {
                    if let Err(e) = config.save() {
                        tracing::warn!(error = %e, "could not normalise config file order");
                    }
                }
            }
            Ok(config)
        } else {
            let config = Self::new()?;
            let parent = path.parent().unwrap();
            if !parent.exists() {
                std::fs::create_dir(parent).context("could not create config directory")?;
            }

            let file = std::fs::File::create(path).context("could not create config file")?;
            serde_json::to_writer_pretty(file, &config)?;
            Ok(config)
        }
    }

    /// Writes the current Config back to the same JSON file `load()` reads from.
    ///
    /// Used when the app discovers a value at runtime (e.g. an auto-detected
    /// GeoIP database path) and wants to persist it so the next start-up
    /// doesn't have to repeat the discovery.  Pretty-printed for readability
    /// since this file is normally edited by hand.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).context("could not create config directory")?;
            }
        }
        let file = std::fs::File::create(&path).context("could not open config file for writing")?;
        serde_json::to_writer_pretty(file, self).context("could not serialise config")?;
        Ok(())
    }

    /// Checks if the string is a valid configured hostname.
    ///
    /// This does *not* include the scheme.
    pub fn is_valid_host(&self, host: &str) -> bool {
        if !self.production {
            return host == "localhost";
        }

        self.domains.iter().any(|s| s == host)
    }

    pub fn canonical_url(&self) -> String {
        let domain = self.domains.first().map(|x| x.as_str()).unwrap_or("localhost");

        // A real deployment is served over HTTPS by the reverse proxy /
        // Cloudflare in front of us, which terminates TLS and forwards to our
        // (usually non-443) listen port. The public URL must therefore be
        // https:// regardless of the local port — deriving the scheme from
        // `server.port` produced http:// links behind a proxy, which made
        // clients like ShareX hit an http→https redirect that downgrades the
        // POST to a GET (405 Method Not Allowed).
        //
        // Local/dev (non-production, or the localhost fallback) keeps plain
        // HTTP on the actual listen port.
        if !self.production || domain == "localhost" {
            return format!("http://localhost:{}", self.server.port);
        }
        format!("https://{domain}")
    }

    pub fn url_to(&self, url: impl Into<std::borrow::Cow<'static, str>>) -> String {
        let mut base = self.canonical_url();
        base.push_str(&url.into());
        base
    }

    /// The OAuth2 callback URL to hand Discord, for both the authorize redirect
    /// and the token exchange (the two must match byte-for-byte). In production
    /// this is the `redirect_uri` registered in the developer portal. In local/dev
    /// it's derived from the live host instead — `http://localhost:<port>/auth/
    /// discord/callback` — so testing on a non-public host (e.g. Windows on
    /// `localhost:9510`) works without swapping config. Register that localhost
    /// URL in the Discord portal as an additional redirect for it to succeed.
    pub fn discord_redirect_uri(&self) -> Option<String> {
        if self.production {
            self.discord.redirect_uri.clone()
        } else {
            Some(self.url_to("/auth/discord/callback"))
        }
    }

    /// The host short links are served from. In production this is the `r.`
    /// subdomain of the primary domain (e.g. `r.klappstuhl.me`); in dev there is
    /// no resolvable subdomain, so it falls back to the local `localhost:<port>`.
    /// Used to recognise inbound short-link requests by their `Host` header.
    pub fn short_domain(&self) -> String {
        let domain = self.domains.first().map(|x| x.as_str()).unwrap_or("localhost");
        if !self.production || domain == "localhost" {
            format!("localhost:{}", self.server.port)
        } else {
            format!("r.{domain}")
        }
    }

    /// The public URL for the short link `code`. Pretty `https://r.<domain>/<code>`
    /// in production; the path-based `http://localhost:<port>/r/<code>` in dev,
    /// where a real `r.` subdomain can't resolve locally.
    pub fn short_link_url(&self, code: &str) -> String {
        let domain = self.domains.first().map(|x| x.as_str()).unwrap_or("localhost");
        if !self.production || domain == "localhost" {
            format!("http://localhost:{}/r/{code}", self.server.port)
        } else {
            format!("https://r.{domain}/{code}")
        }
    }

    /// The `Domain` attribute to set on the auth/session cookie, so a login on the
    /// apex (`klappstuhl.me`) is also presented to the dashboard subdomain
    /// (`percy.klappstuhl.me`) — without it the host-only cookie never reaches the
    /// subdomain and dashboard users appear logged out there. Resolves to the
    /// primary domain (covers it plus every subdomain), or `localhost` in dev
    /// (covers `percy.localhost`). Dev caveat: accessing the apex via a raw IP
    /// (e.g. `127.0.0.1`) instead of `localhost` makes the browser reject the
    /// `Domain=localhost` cookie — use `localhost` in dev.
    /// The `Domain` attribute for the auth cookie. `None` in dev (host-only
    /// cookie — browsers reject cross-subdomain `Domain=localhost`).
    /// `Some("klappstuhl.me")` in production so the cookie is shared between
    /// the apex and the percy subdomain.
    pub fn cookie_domain(&self) -> Option<String> {
        self.domains.first().map(|x| x.to_string())
    }

    /// The registrable domain for `safe_next_for_domain` trusted-host checks.
    /// Returns `"localhost"` in dev, the configured domain in production.
    pub fn trusted_domain(&self) -> String {
        self.domains
            .first()
            .map(|x| x.as_str())
            .unwrap_or("localhost")
            .to_string()
    }

    /// The public URL for the Percy subdomain. In production this is
    /// `https://percy.<domain>`; in dev it's `http://percy.localhost:<port>`.
    /// Used in templates so links from the main site to Percy are always correct.
    pub fn percy_url(&self) -> String {
        let domain = self.domains.first().map(|x| x.as_str()).unwrap_or("localhost");
        if !self.production || domain == "localhost" {
            format!("http://percy.localhost:{}", self.server.port)
        } else {
            format!("https://percy.{domain}")
        }
    }
}

/// One-time, in-memory migration from the old flat config keys to the grouped
/// sub-maps. Each old key is moved into `group.new_key` (without clobbering a
/// value already present there), so an upgraded install keeps its settings and
/// the file is rewritten in grouped form by `load()`'s normalisation step.
fn migrate_flat_to_grouped(value: &mut serde_json::Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    // (old flat key, group, new key within the group)
    //
    // Only keys that still land somewhere are worth moving. The table used to
    // also regroup the flat `cloudflare_*`, `proxy_*`, `backup_*` and the other
    // alert sinks; those groups left with the admin control plane, so a config
    // still carrying them now falls through to the same place any unknown key
    // does — ignored on load, dropped on the next re-normalise.
    const MOVES: &[(&str, &str, &str)] = &[("discord_webhook_url", "alerts", "discord_webhook_url")];

    for (old, group, new) in MOVES {
        let Some(moved) = obj.remove(*old) else {
            continue;
        };
        let entry = obj
            .entry(*group)
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(map) = entry.as_object_mut() {
            map.entry(*new).or_insert(moved);
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_ip")]
    pub ip: IpAddr,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_ip() -> IpAddr {
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}

fn default_port() -> u16 {
    9510
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            ip: default_ip(),
            port: default_port(),
        }
    }
}

impl ServerConfig {
    pub fn address(&self) -> SocketAddr {
        SocketAddr::from((self.ip, self.port))
    }
}

/// A global variable for the loaded config.
///
/// Currently mainly used for templates
pub static CONFIG: OnceLock<Config> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn migrates_the_legacy_flat_webhook_key_into_the_alerts_group() {
        let mut v = json!({
            "production": true,
            "discord_webhook_url": "https://discord/x",
        });
        migrate_flat_to_grouped(&mut v);

        assert_eq!(v["alerts"]["discord_webhook_url"], "https://discord/x");
        // Untouched, and the old flat key is gone.
        assert_eq!(v["production"], true);
        assert!(v.get("discord_webhook_url").is_none());
    }

    /// A config still carrying the admin control plane's keys loads fine — they
    /// are simply not moved anywhere, and serde ignores them (there is no
    /// `deny_unknown_fields`), so the next re-normalise drops them. An operator
    /// upgrading past the Vantage split must not hit a parse error.
    #[test]
    fn legacy_admin_keys_are_left_alone_rather_than_regrouped() {
        let mut v = json!({
            "production": true,
            "cloudflare_api_token": "tok",
            "proxy_kind": "cloudflared",
            "backup_keep": 7,
            "alert_webhook_url": "https://hook",
        });
        migrate_flat_to_grouped(&mut v);

        // No group is conjured for a key nothing reads any more.
        assert!(v.get("cloudflare").is_none());
        assert!(v.get("proxy").is_none());
        assert!(v.get("backup").is_none());
        assert_eq!(v["production"], true);

        // And the whole thing still deserializes into the trimmed Config.
        v["secret_key"] = json!(SecretKey::random().unwrap());
        serde_json::from_value::<Config>(v).expect("a config with stale admin keys must still load");
    }

    #[test]
    fn migration_preserves_an_already_grouped_value() {
        let mut v = json!({
            "alerts": { "discord_webhook_url": "new" },
            "discord_webhook_url": "old",
        });
        migrate_flat_to_grouped(&mut v);
        assert_eq!(v["alerts"]["discord_webhook_url"], "new"); // grouped wins
        assert!(v.get("discord_webhook_url").is_none());
    }

    #[test]
    fn migration_is_noop_for_already_grouped_config() {
        let mut v = json!({ "alerts": { "discord_webhook_url": "x" }, "paste": { "anonymous": true } });
        let before = v.clone();
        migrate_flat_to_grouped(&mut v);
        assert_eq!(v, before);
    }

    /// The on-disk key order must match the documented README example. serde
    /// serialises in field-declaration order, so this guards the two from
    /// drifting apart.
    #[test]
    fn serialises_in_canonical_grouped_order() {
        let json = serde_json::to_string(&Config::new().unwrap()).unwrap();
        // The full top-level list, in the order `docs/setup.md` shows it. Listing
        // every key (not just the first few) is what makes this catch a *removed*
        // one: the admin keys were still in the docs long after they were gone.
        let expected = [
            "production",
            "domains",
            "server",
            "secret_key",
            "alerts",
            "clamav_addr",
            "virustotal_api_key",
            "chromium_path",
            "ffmpeg_path",
            "max_upload_bytes",
            "paste",
            "discord",
            "sso_secret",
            "gallery_provision_token",
        ];
        let mut last = 0usize;
        for key in expected {
            let idx = json
                .find(&format!("\"{key}\""))
                .unwrap_or_else(|| panic!("missing top-level key {key}"));
            assert!(idx >= last, "key `{key}` is out of canonical order");
            last = idx;
        }
    }
}
