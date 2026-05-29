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
}

/// The server configuration.
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
    /// The Discord webhook URL for audit log announcements.
    #[serde(rename = "discord_webhook_url")]
    #[serde(default)]
    pub webhook: Option<Webhook>,
    /// Services to monitor on the `/admin/services` admin page.
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    /// The server IP and port configuration
    #[serde(default)]
    pub server: ServerConfig,
    /// Path to a MaxMind GeoLite2-City `.mmdb` file used by `/admin/security`.
    /// Defaults to `<data>/geoip/GeoLite2-City.mmdb` if unset. Optional — if
    /// the file is missing, IP lookups quietly degrade to "Unknown".
    #[serde(default)]
    pub geoip_db_path: Option<PathBuf>,
    /// Cloudflare API token (with read access to zone analytics). When set
    /// alongside `cloudflare_zone_id`, the security dashboard adds the
    /// "Cloudflare" section with traffic / threat / WAF panels.
    #[serde(default)]
    pub cloudflare_api_token: Option<String>,
    /// Cloudflare zone ID for the domain this app sits behind.
    #[serde(default)]
    pub cloudflare_zone_id: Option<String>,
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
    /// PostgreSQL connection string for the `/admin/postgres` page.
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
    /// Directory that proxy configuration is written into. When set, the
    /// `/admin/proxy` page renders an nginx (or caddy) server block per
    /// route and writes it to `<dir>/<subdomain>.conf`.  When unset, the
    /// page still manages routes in the DB but does not touch disk —
    /// useful when the proxy is hand-managed and you just want a record
    /// of which subdomain maps to which container.
    #[serde(default)]
    pub proxy_config_dir: Option<PathBuf>,
    /// Which proxy syntax to emit.  `"nginx"` (default) writes nginx
    /// `server { ... }` blocks; `"caddy"` writes Caddyfile entries.
    #[serde(default)]
    pub proxy_kind: Option<String>,
    /// Shell command run after the config files are regenerated. Typical
    /// values: `"nginx -s reload"`, `"systemctl reload nginx"`,
    /// `"caddy reload --config /etc/caddy/Caddyfile"`. Skipped when unset.
    #[serde(default)]
    pub proxy_reload_command: Option<String>,
    /// Hours between automatic SQLite backups (`VACUUM INTO`). `0` disables
    /// the scheduler. Defaults to 24 when unset.
    #[serde(default)]
    pub backup_interval_hours: Option<u64>,
    /// Number of automatic backups to retain; older ones are pruned.
    /// Defaults to 14 when unset.
    #[serde(default)]
    pub backup_keep: Option<usize>,
    /// The secret key used for all crypto related functionality in the server.
    ///
    /// Microbenching makes it evident that cloning this without an Arc is around ~4x faster.
    pub secret_key: SecretKey,
}

impl Config {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            production: false,
            domains: Vec::new(),
            webhook: None,
            services: Vec::new(),
            server: ServerConfig::default(),
            geoip_db_path: None,
            cloudflare_api_token: None,
            cloudflare_zone_id: None,
            secret_scan_paths: Vec::new(),
            postgres_url: None,
            clamav_addr: None,
            virustotal_api_key: None,
            spotlight_scripts: Vec::new(),
            sshd_auth_log_path: None,
            firewall_backend: None,
            proxy_config_dir: None,
            proxy_kind: None,
            proxy_reload_command: None,
            backup_interval_hours: None,
            backup_keep: None,
            secret_key: SecretKey::random()?,
        })
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
            let file = std::fs::read_to_string(path).context("could not read config file")?;
            serde_json::from_str(&file).context("could not parse config file")
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
        let scheme = if self.server.port == 443 { "https://" } else { "http://" };
        let domain = self.domains.first().map(|x| x.as_str()).unwrap_or("localhost");
        let mut url = String::with_capacity(8 + domain.len());
        url.push_str(scheme);
        url.push_str(domain);
        if domain == "localhost" {
            url.push(':');
            url.push_str(&self.server.port.to_string());
        }
        url
    }

    pub fn url_to(&self, url: impl Into<std::borrow::Cow<'static, str>>) -> String {
        let mut base = self.canonical_url();
        base.push_str(&url.into());
        base
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