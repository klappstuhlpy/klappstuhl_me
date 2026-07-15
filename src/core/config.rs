use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::OnceLock,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::key::SecretKey;
use crate::{cli::PROGRAM_NAME, discord::Webhook};

/// Configuration for a single Docker service shown on the `/admin/docker` page.
///
/// Note: the legacy `kind` field (previously `"docker"` or `"screen"`) is silently
/// ignored if present in the config file — all services are now Docker-only.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServiceConfig {
    /// Human-readable display name.
    pub name: String,
    /// The Docker container name passed to `docker start` / `docker stop` etc.
    pub identifier: String,
    /// Working directory that contains the `docker-compose.yml`.
    ///
    /// When set, Start/Stop/Restart run `docker compose up -d` /
    /// `docker compose down` / `docker compose restart` in this directory
    /// instead of the plain `docker start` / `docker stop` / `docker restart`
    /// commands.
    ///
    /// Example config:
    /// ```json
    /// {
    ///   "name": "My App",
    ///   "identifier": "my_app",
    ///   "path": "/home/user/my-app"
    /// }
    /// ```
    #[serde(default)]
    pub path: Option<String>,
}

/// A pre-defined script the Spotlight palette can invoke.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SpotlightScript {
    /// Unique identifier used by the run endpoint.
    pub id: String,
    /// Human-readable name shown in the palette.
    pub name: String,
    /// Shell command to execute (passed to `sh -c` on Unix, `cmd /C` on Windows).
    pub command: String,
    /// Optional description shown as subtitle.
    #[serde(default)]
    pub description: Option<String>,
    /// Working directory for the command. Defaults to the process cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional 5-field cron schedule (`min hour dom month dow`, evaluated in
    /// UTC). When set, a background scheduler runs the script automatically at
    /// the matching times, in addition to on-demand runs from the Ctrl+K
    /// palette. Supports `*`, lists (`1,15`), ranges (`9-17`), and steps
    /// (`*/15`). Example: `"0 4 * * *"` runs daily at 04:00 UTC.
    #[serde(default)]
    pub schedule: Option<String>,
}

/// Off-site backup target. When set, every freshly created SQLite backup is
/// also uploaded to an S3-compatible object store (AWS S3, Backblaze B2,
/// Cloudflare R2, MinIO, …) so a dead local disk can't take the backups with
/// it. Uses path-style addressing + AWS Signature V4, which all of the above
/// accept — no extra binary required.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackupRemoteConfig {
    /// Storage backend. Currently only `"s3"` is supported.
    #[serde(default = "default_remote_kind")]
    pub kind: String,
    /// Endpoint base URL, e.g. `"https://s3.us-west-002.backblazeb2.com"`,
    /// `"https://<account>.r2.cloudflarestorage.com"`, or for AWS
    /// `"https://s3.us-east-1.amazonaws.com"`.
    pub endpoint: String,
    /// Signing region. AWS needs the real region; B2/R2/MinIO accept any value
    /// (defaults to `us-east-1`).
    #[serde(default = "default_remote_region")]
    pub region: String,
    /// Destination bucket name.
    pub bucket: String,
    /// Optional key prefix inside the bucket (e.g. `"klappstuhl/"`). A trailing
    /// slash is added automatically if missing and the prefix is non-empty.
    #[serde(default)]
    pub prefix: String,
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
}

fn default_remote_kind() -> String {
    "s3".to_string()
}

fn default_remote_region() -> String {
    "us-east-1".to_string()
}

impl BackupRemoteConfig {
    /// The key prefix normalised to either empty or ending in a single `/`.
    pub fn normalized_prefix(&self) -> String {
        let p = self.prefix.trim_matches('/');
        if p.is_empty() {
            String::new()
        } else {
            format!("{p}/")
        }
    }
}

/// Alert delivery sinks. A metric / health / secret / backup alert fans out to
/// every one of these that is set.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AlertsConfig {
    /// Discord incoming-webhook URL.
    #[serde(default)]
    pub discord_webhook_url: Option<Webhook>,
    /// ntfy topic URL (e.g. `https://ntfy.sh/my-topic`) — alerts are pushed as
    /// plain text.
    #[serde(default)]
    pub ntfy_url: Option<String>,
    /// Generic webhook URL — alerts are POSTed as a neutral JSON body
    /// `{title, level, body, fields}`.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// SMTP email sink — when set, alerts are also delivered as a plain-text
    /// email to every recipient.
    #[serde(default)]
    pub email: Option<EmailConfig>,
}

/// SMTP delivery settings for the email alert sink. TLS is mandatory: port
/// 465 uses implicit TLS, any other port (587, 25) upgrades via STARTTLS.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmailConfig {
    /// SMTP server hostname (e.g. `smtp.fastmail.com`).
    pub host: String,
    /// SMTP port. `465` → implicit TLS, otherwise STARTTLS. Defaults to 587.
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// AUTH LOGIN username. Omit (with `password`) for an unauthenticated relay.
    #[serde(default)]
    pub username: Option<String>,
    /// AUTH LOGIN password / app-password.
    #[serde(default)]
    pub password: Option<String>,
    /// Envelope sender / `From:` address.
    pub from: String,
    /// One or more recipient addresses.
    pub to: Vec<String>,
}

fn default_smtp_port() -> u16 {
    587
}

/// Cloudflare credentials and tunnel settings. The token + zone power the
/// security dashboard's Cloudflare panels; the token + `account_id` +
/// `tunnel_id` additionally let `/admin/proxy` manage a remotely-managed
/// Cloudflare Tunnel's ingress over the API.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CloudflareConfig {
    /// API token. `Zone.Analytics:Read` for the security panels; for tunnel
    /// management it also needs Account › Cloudflare Tunnel › Edit and
    /// Zone › DNS › Edit.
    #[serde(default)]
    pub api_token: Option<String>,
    /// Zone ID for the domain this app sits behind.
    #[serde(default)]
    pub zone_id: Option<String>,
    /// Account ID — required for tunnel-API management.
    #[serde(default)]
    pub account_id: Option<String>,
    /// UUID of the Cloudflare Tunnel to manage over the API (the right model
    /// for a dashboard/remotely-managed tunnel with no local credentials file).
    #[serde(default)]
    pub tunnel_id: Option<String>,
    /// Local-file mode only: tunnel id/name written as `tunnel:` into a
    /// generated `config.yml`. Used when the tunnel API isn't configured.
    #[serde(default)]
    pub tunnel_name: Option<String>,
    /// Local-file mode only: path written as `credentials-file:` into the
    /// generated `config.yml`.
    #[serde(default)]
    pub tunnel_credentials_file: Option<String>,
}

/// Reverse-proxy / domain-manager settings for `/admin/proxy`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProxyConfig {
    /// Config syntax to emit: `"nginx"` (default), `"caddy"`, or
    /// `"cloudflared"`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Directory generated config is written into. Unset = DB-only (and unused
    /// in cloudflared tunnel-API mode).
    #[serde(default)]
    pub config_dir: Option<PathBuf>,
    /// Shell command run after config is regenerated, e.g. `"nginx -s reload"`.
    /// Skipped when unset / in tunnel-API mode.
    #[serde(default)]
    pub reload_command: Option<String>,
}

/// SQLite backup settings.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BackupConfig {
    /// Hours between automatic `VACUUM INTO` backups. `0` disables; unset
    /// defaults to 24.
    #[serde(default)]
    pub interval_hours: Option<u64>,
    /// Number of automatic backups to retain. Unset defaults to 14.
    #[serde(default)]
    pub keep: Option<usize>,
    /// Off-site backup target. Unset = local-only backups.
    #[serde(default)]
    pub remote: Option<BackupRemoteConfig>,
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

/// AI assistant settings powering the "Ask the AI" feature (admin Spotlight).
/// Backed by the Groq API (free tier, available in the EU; OpenAI-compatible).
/// Disabled (the endpoint returns 503) unless `api_key` is set.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AiConfig {
    /// Groq API key (`gsk_…`, free from <https://console.groq.com/keys>). The
    /// key stays server-side; the browser only ever talks to this app's
    /// `/api/ask` proxy. Unset = feature off.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Groq model id to use. Unset defaults to a free model with tool-calling
    /// support (see [`AiConfig::model_id`]).
    #[serde(default)]
    pub model: Option<String>,
    /// Default for "may anyone spend tokens?": `false` restricts `/api/ask` to
    /// admin accounts, `true` lets any visitor use it (still rate-limited).
    /// This is only the *initial* value — admins can flip it at runtime via
    /// `POST /admin/ai/public` (persisted in the `storage` KV table), and the
    /// stored value then takes precedence over this default.
    #[serde(default)]
    pub public: bool,
}

impl AiConfig {
    /// Whether the ask-AI feature is enabled (an API key is configured).
    pub fn enabled(&self) -> bool {
        self.api_key.as_deref().map(|k| !k.is_empty()).unwrap_or(false)
    }

    /// The configured model, or a free default with reliable tool-calling.
    pub fn model_id(&self) -> &str {
        self.model
            .as_deref()
            .filter(|m| !m.is_empty())
            .unwrap_or("llama-3.3-70b-versatile")
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
    /// Alert delivery sinks (Discord / ntfy / generic webhook).
    #[serde(default)]
    pub alerts: AlertsConfig,
    /// Cloudflare credentials + tunnel settings.
    #[serde(default)]
    pub cloudflare: CloudflareConfig,
    /// Reverse-proxy / domain-manager settings.
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// SQLite backup settings.
    #[serde(default)]
    pub backup: BackupConfig,
    /// Services to monitor on the `/admin/docker` admin page.
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    /// Path to a MaxMind GeoLite2-City `.mmdb` file used by `/admin/security`.
    /// Defaults to `<data>/geoip/GeoLite2-City.mmdb` if unset. Optional — if
    /// the file is missing, IP lookups quietly degrade to "Unknown".
    #[serde(default)]
    pub geoip_db_path: Option<PathBuf>,
    /// Directories to scan for leaked secrets (API keys, tokens, private
    /// keys, etc.).  When empty, the scheduled scanner is disabled — the
    /// /admin/secrets page still loads and a manual scan can be triggered.
    ///
    /// Recursive: subdirectories are walked.  Binary files, files larger
    /// than 1 MB, and common build directories (.git, node_modules, target,
    /// dist) are skipped automatically.
    ///
    /// **Docker note:** paths are resolved from inside the container, so
    /// host paths like `/home/alice/code` don't work directly. The default
    /// compose file already bind-mounts `/home` and `/root` (for the SSH
    /// admin page); reuse those by prefixing with `/host-home` or
    /// `/host-root`, e.g. `"/host-home/alice/code"`. For paths elsewhere
    /// on the host, add a corresponding `:ro` bind mount in
    /// `docker-compose.yml` and reference the in-container path here.
    #[serde(default)]
    pub secret_scan_paths: Vec<PathBuf>,
    /// PostgreSQL connection string for the `/admin/database` page.
    ///
    /// Format (libpq URL):
    /// `postgresql://user:password@host:port/database`
    ///
    /// Leave unset to disable the page. The configured account should have
    /// at least read access to `pg_catalog` and connection rights to every
    /// database you want to browse — typically the `postgres` superuser
    /// (or a dedicated read-only role with `pg_read_all_data`).
    ///
    /// Safe-mode queries are wrapped in `BEGIN READ ONLY` so even
    /// privileged credentials can't accidentally mutate state through the
    /// query runner unless the operator explicitly opts in to danger mode.
    #[serde(default)]
    pub postgres_url: Option<String>,
    /// ClamAV daemon address (e.g. `"127.0.0.1:3310"`).
    /// When set, the file sanitizer page connects to clamd for virus scanning.
    #[serde(default)]
    pub clamav_addr: Option<String>,
    /// VirusTotal public API key.
    /// When set, the file sanitizer checks each file's SHA-256 against VT.
    #[serde(default)]
    pub virustotal_api_key: Option<String>,
    /// Pre-defined scripts the Spotlight palette can run.
    /// Each entry needs a unique `id`, a display `name`, and a shell `command`.
    #[serde(default)]
    pub spotlight_scripts: Vec<SpotlightScript>,
    /// Optional path to an sshd auth log (typically `/var/log/auth.log` on
    /// Debian/Ubuntu, or `/var/log/secure` on RHEL). When set, a background
    /// task tails the file and updates `ssh_key.last_used_at` whenever a
    /// successful publickey authentication line is observed whose
    /// `SHA256:<fingerprint>` matches a stored key.
    ///
    /// Requires sshd to log fingerprints (default on modern OpenSSH; if your
    /// logs only show the user/IP without `ssh2: <algo> SHA256:<fp>`, raise
    /// `LogLevel` to `VERBOSE` in `/etc/ssh/sshd_config`).
    ///
    /// In the default Docker setup, bind-mount the host log read-only
    /// (e.g. `- /var/log/auth.log:/host-log/auth.log:ro`) and set this to
    /// `/host-log/auth.log`. When unset, `last_used_at` stays NULL.
    #[serde(default)]
    pub sshd_auth_log_path: Option<PathBuf>,
    /// Forces a specific firewall backend at start-up. Valid values:
    /// `"nftables"`, `"ufw"`, `"iptables"`, `"disabled"`. When unset, the
    /// `/admin/firewall` page probes each backend in order and uses the
    /// first one that responds.  Set to `"disabled"` to keep the UI but
    /// stop the server from issuing real packet-filter commands (useful
    /// in dev or when running without `NET_ADMIN`).
    #[serde(default)]
    pub firewall_backend: Option<String>,
    /// Hours between background container image-update checks (queries each
    /// configured service's registry for a newer digest). `0` disables the
    /// checker. Defaults to 12 when unset.
    #[serde(default)]
    pub update_check_interval_hours: Option<u64>,
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
    /// AI assistant settings (Groq) for the "Ask the AI" feature.
    /// Off unless `ai.api_key` is set.
    #[serde(default)]
    pub ai: AiConfig,
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
            cloudflare: CloudflareConfig::default(),
            proxy: ProxyConfig::default(),
            backup: BackupConfig::default(),
            services: Vec::new(),
            geoip_db_path: None,
            secret_scan_paths: Vec::new(),
            postgres_url: None,
            clamav_addr: None,
            virustotal_api_key: None,
            spotlight_scripts: Vec::new(),
            sshd_auth_log_path: None,
            firewall_backend: None,
            update_check_interval_hours: None,
            chromium_path: None,
            ffmpeg_path: None,
            max_upload_bytes: None,
            ai: AiConfig::default(),
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
    const MOVES: &[(&str, &str, &str)] = &[
        ("discord_webhook_url", "alerts", "discord_webhook_url"),
        ("ntfy_url", "alerts", "ntfy_url"),
        ("alert_webhook_url", "alerts", "webhook_url"),
        ("cloudflare_api_token", "cloudflare", "api_token"),
        ("cloudflare_zone_id", "cloudflare", "zone_id"),
        ("cloudflare_account_id", "cloudflare", "account_id"),
        ("cloudflared_tunnel_id", "cloudflare", "tunnel_id"),
        ("cloudflared_tunnel", "cloudflare", "tunnel_name"),
        ("cloudflared_credentials_file", "cloudflare", "tunnel_credentials_file"),
        ("proxy_kind", "proxy", "kind"),
        ("proxy_config_dir", "proxy", "config_dir"),
        ("proxy_reload_command", "proxy", "reload_command"),
        ("backup_interval_hours", "backup", "interval_hours"),
        ("backup_keep", "backup", "keep"),
        ("backup_remote", "backup", "remote"),
    ];

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
    fn migrates_legacy_flat_keys_into_groups() {
        let mut v = json!({
            "production": true,
            "cloudflare_api_token": "tok",
            "cloudflare_zone_id": "zone",
            "cloudflare_account_id": "acct",
            "cloudflared_tunnel_id": "uuid",
            "cloudflared_tunnel": "my-tunnel",
            "cloudflared_credentials_file": "/etc/cf/x.json",
            "proxy_kind": "cloudflared",
            "proxy_config_dir": "/etc/cf",
            "proxy_reload_command": "true",
            "backup_interval_hours": 12,
            "backup_keep": 7,
            "discord_webhook_url": "https://discord/x",
            "ntfy_url": "https://ntfy.sh/t",
            "alert_webhook_url": "https://hook",
        });
        migrate_flat_to_grouped(&mut v);

        assert_eq!(v["cloudflare"]["api_token"], "tok");
        assert_eq!(v["cloudflare"]["zone_id"], "zone");
        assert_eq!(v["cloudflare"]["account_id"], "acct");
        assert_eq!(v["cloudflare"]["tunnel_id"], "uuid");
        assert_eq!(v["cloudflare"]["tunnel_name"], "my-tunnel");
        assert_eq!(v["cloudflare"]["tunnel_credentials_file"], "/etc/cf/x.json");
        assert_eq!(v["proxy"]["kind"], "cloudflared");
        assert_eq!(v["proxy"]["config_dir"], "/etc/cf");
        assert_eq!(v["proxy"]["reload_command"], "true");
        assert_eq!(v["backup"]["interval_hours"], 12);
        assert_eq!(v["backup"]["keep"], 7);
        assert_eq!(v["alerts"]["discord_webhook_url"], "https://discord/x");
        assert_eq!(v["alerts"]["ntfy_url"], "https://ntfy.sh/t");
        assert_eq!(v["alerts"]["webhook_url"], "https://hook");

        // Untouched + old flat keys removed.
        assert_eq!(v["production"], true);
        for old in ["cloudflare_api_token", "proxy_kind", "backup_keep", "alert_webhook_url"] {
            assert!(v.get(old).is_none(), "{old} should have been moved");
        }
    }

    #[test]
    fn migration_preserves_an_already_grouped_value() {
        let mut v = json!({
            "cloudflare": { "api_token": "new" },
            "cloudflare_api_token": "old",
        });
        migrate_flat_to_grouped(&mut v);
        assert_eq!(v["cloudflare"]["api_token"], "new"); // grouped wins
        assert!(v.get("cloudflare_api_token").is_none());
    }

    #[test]
    fn migration_is_noop_for_already_grouped_config() {
        let mut v = json!({ "cloudflare": { "api_token": "x" }, "proxy": { "kind": "nginx" } });
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
        let expected = [
            "production",
            "domains",
            "server",
            "secret_key",
            "alerts",
            "cloudflare",
            "proxy",
            "backup",
            "services",
            "geoip_db_path",
            "secret_scan_paths",
            "postgres_url",
            "clamav_addr",
            "virustotal_api_key",
            "spotlight_scripts",
            "sshd_auth_log_path",
            "firewall_backend",
            "update_check_interval_hours",
            "chromium_path",
            "ffmpeg_path",
            "max_upload_bytes",
            "ai",
            "paste",
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
