# Klappstuhl.me

Personal website, image-hosting service, and homelab admin platform built with Rust.

What started as a small image host has grown into a single binary that also runs
the admin tooling for the machine it sits on: live system metrics, container
management, security analytics, virus scanning, and an invite-only user system.

## Highlights

- **Public site** — landing page, projects page, image gallery.
- **Image host** — drag-and-drop upload, public direct links, per-user library.
- **Admin shell at `/admin`** — sidebar layout with the following pages:
  - **Dashboard** — request analytics, popular routes, referring sites, API consumers.
  - **Invites** — invite-only signup. Admins generate one-time codes (with optional expiry + note), copy a `/signup?code=…` URL, share it with the invitee. No public registration.
  - **Docker** — combined services dashboard and dependency graph.
    - Per-service cards for every entry in `config.services`: live running/offline badge, uptime, image name, short container ID, restart count, live CPU % and RAM bar (auto-refreshed every 15 s). Start / Stop / Restart / Pull image / Recreate actions.
    - Per-service live log console streamed over Server-Sent Events with syntax highlighting (timestamps, log levels, HTTP methods, status codes, IPs, durations, strings).
    - Interactive Cytoscape.js dependency graph showing containers, networks, and volumes with edges for network membership, volume mounts, and `depends_on` links. Click any node for an inline side-panel with full inspect data. Filter by kind, re-layout, fit-to-screen. Graph updates softly (no layout re-run) when a WS Docker event arrives.
    - Live Events strip at the bottom of the graph — WebSocket feed of Docker daemon events (start / stop / die / create / destroy / …).
    - Quick link to the Snapshots page.
  - **Snapshots** — point-in-time container image snapshots via `docker commit`. Capture any running container from the graph side-panel, give it an optional description; later restore it as a new named container in one click. Stored in SQLite; the committed image tag is a nanoid-generated string. Deletion removes both the DB row and the image (`docker rmi`).
  - **Metrics** — live host stats (CPU, RAM, disk usage, disk I/O, network throughput) plus per-container Docker stats. uPlot charts with selectable ranges (1h / 6h / 24h / 7d / 30d). Threshold alerts fire a Discord webhook on `OK → ALERT` transitions with a 30-minute cooldown.
  - **Security** — failed logins, top offending IPs (with GeoIP country/city), reason breakdown, country distribution, recent activity feed. Optional Cloudflare panels (zone analytics + WAF events) when an API token + zone ID are configured.
  - **Secrets** — periodic + on-demand filesystem scanner with 18 built-in rules (AWS / GitHub / Stripe / OpenAI / Anthropic / Discord / Slack tokens, PEM private keys, JWTs, DB URLs). Findings stored deduplicated with first/last-seen tracking; dismiss / resolve / reopen workflow. Discord webhook on new criticals.
  - **Audit log** — every state-changing action records actor, action, target, IP, and a JSON `meta` blob. Auth events (login success/fail, signup, password change, logout), invite create/revoke, service/snapshot/script actions, secret status changes, and admin cache invalidation are all tracked. Filterable by action prefix and actor.
  - **Postgres** — browse databases, tables, and roles on a separate PostgreSQL server. Safe-mode query runner wraps every statement in `BEGIN TRANSACTION READ ONLY` so even a superuser credential can't accidentally mutate state; explicit danger-mode toggle for `INSERT` / `UPDATE` / `DELETE` / DDL when needed. Every query writes an audit entry (including blocked-by-safe-mode attempts).
  - **SSH Keys** — manage authorized SSH public keys stored in the database. Add keys with an optional label; optionally sync them to a real `authorized_keys` file on the host and populate each key's "Last used" timestamp by tailing the sshd auth log (see [SSH key sync](#ssh-key-sync-and-last-used-tracking)). Token audit (active API tokens across accounts) and session audit (active login sessions with IP + user-agent) sub-pages. Revoke individual keys, tokens, or sessions.
  - **File Sanitizer** — drag-and-drop file scanning with two optional backends.
    - **ClamAV** — streams the uploaded file to a local `clamd` daemon over TCP (INSTREAM protocol). Reports virus name on detection.
    - **VirusTotal** — looks up the file's SHA-256 against the VT v3 API (no file upload; hash-only). Shows `N/M engines detected` with a link to the VT report.
    - Scan history table (SQLite) with per-row deletion. Both backends are independent; results combine into a single clean/infected/unknown verdict.
- **Spotlight palette (Ctrl+K)** — macOS-style command palette available on every admin page. Opens with `Ctrl+K` (or the Search button in the sidebar footer). Fuzzy-searches across all admin nav items, configured scripts, audit log entries, file scan history, SSH keys, and live Docker containers. Keyboard-navigable (↑/↓/Enter/Esc). Scripts run inline and show their stdout/stderr output in the palette without leaving the page.
- **API tokens with scopes** — generated from `/account`, each token can be restricted to a subset of `images:read · images:write · admin:read · admin:write`. Legacy keys (created without selecting scopes) keep full access for back-compat.
- **Live updates over WebSocket** — `/ws` push topic events to dashboards. Metrics tiles refresh on every scrape, audit-log entries appear instantly, and Docker graph updates on container events; polling is the automatic fallback when the socket is closed.
- **Installable PWA** — `site.webmanifest` + service worker shell-cache the static assets. iOS standalone-mode meta tags + a black theme colour so the app looks native when installed to the home screen. Network-only for everything dynamic; offline access is intentionally **not** a goal for a homelab admin tool.
- **REST API** — OpenAPI 3.0 documented at `/api/docs` via utoipa + Scalar.
- **Automatic TLS** — Let's Encrypt via rustls-acme (TLS-ALPN-01) in production mode.

## Tech stack

- **Backend** — Rust, [Axum](https://github.com/tokio-rs/axum) 0.7, Tokio, SQLite (rusqlite, bundled).
- **Templates** — [Askama](https://github.com/djc/askama) (compile-time server-side rendering).
- **Storage** — two SQLite files in the data dir: `main.db` (accounts, sessions, images, invites, metrics, snapshots, SSH keys, file scans), `requests.db` (HTTP access log).
- **Auth** — HMAC-signed session tokens, Argon2 password hashing.
- **TLS** — Automatic Let's Encrypt via rustls-acme.
- **Metrics** — Native `/proc` and `/sys` parsing, `df` for disk usage, `docker stats` for containers. uPlot for charts.
- **GeoIP** — Offline [MaxMind GeoLite2-City](https://dev.maxmind.com/geoip/geolite2-free-geolocation-data) database via the `maxminddb` crate.
- **Cloudflare** — GraphQL Analytics API (zone traffic + firewall events) via `reqwest`.
- **Docker** — [bollard](https://github.com/fussybeaver/bollard) crate for the Docker daemon API (container list, inspect, events stream, `docker commit`). [Cytoscape.js](https://js.cytoscape.org/) for the dependency graph.
- **Virus scanning** — inline ClamAV INSTREAM TCP client; VirusTotal v3 REST API via `reqwest`. SHA-256 via the `sha2` crate.

## Quick start (Docker — recommended)

The repo ships a multi-stage `Dockerfile` and `docker-compose.yml` configured for a production deployment with host-metric visibility.

```bash
# 1. Start the container once so it writes a default config:
docker compose up -d
docker compose down

# 2. Edit ./data/config/klappstuhl_me/config.json (see "Configuration" below).

# 3. Create the first admin account (interactive prompt):
docker compose run --rm klappstuhl_me ./klappstuhl_me admin

# 4. Start permanently:
docker compose up -d
```

After that, log in at `https://yourdomain.com/login`, then visit `/admin` to access the dashboard.

### What the compose file mounts

| Mount                                       | Why                                                                                          |
|---------------------------------------------|----------------------------------------------------------------------------------------------|
| `./data:/data`                              | Persistent config, database, logs, ACME cert cache.                                          |
| `/var/run/docker.sock`                      | So `/admin/docker` can run `docker ps` / `docker compose up -d`.                             |
| `/proc:/host/proc:ro`                       | So `/admin/metrics` reports the **host's** CPU/RAM/network.                                  |
| `/sys:/host/sys:ro`                         | Same — for `/sys/block/*` (disk I/O counters).                                               |
| `/home/<user>/.ssh:/host-ssh`               | (Optional) Target user's `.ssh` dir, so the SSH admin page can write `authorized_keys` here. |
| `/var/log/auth.log:/host-log/auth.log:ro`   | (Optional) Host sshd log, so the SSH admin page can populate `last_used_at`.                 |

`HOST_PROC=/host/proc` and `HOST_SYS=/host/sys` are pre-set in the compose file so the metrics collector picks up the host filesystem.

## Configuration

A default `config.json` is written on first start. Full layout with all optional fields:

```json
{
  "production": false,
  "domains": ["yourdomain.com"],
  "server": {
    "ip": "0.0.0.0",
    "port": 443
  },
  "secret_key": "<auto-generated — leave this alone>",
  "discord_webhook_url": null,
  "services": [],
  "geoip_db_path": null,
  "cloudflare_api_token": null,
  "cloudflare_zone_id": null,
  "secret_scan_paths": [],
  "postgres_url": null,
  "clamav_addr": null,
  "virustotal_api_key": null,
  "spotlight_scripts": [],
  "authorized_keys_path": null,
  "sshd_auth_log_path": null
}
```

| Key                      | Type              | Notes                                                                                          |
|--------------------------|-------------------|------------------------------------------------------------------------------------------------|
| `production`             | bool              | `true` switches the server to TLS via Let's Encrypt on port 443.                               |
| `domains`                | string[]          | Hostnames that ACME will request certificates for.                                             |
| `server.port`            | u16               | Listen port (`443` in production, anything else for dev).                                      |
| `server.ip`              | string            | The IP the server listens on (default `0.0.0.0`).                                              |
| `secret_key`             | string            | Auto-generated; used for HMAC signing of session tokens and flash cookies.                     |
| `discord_webhook_url`    | string \| null    | Discord incoming webhook URL for metric alerts and secret findings.                            |
| `services`               | ServiceConfig[]   | Docker services shown on `/admin/docker`. See below.                                           |
| `geoip_db_path`          | string \| null    | Path to a GeoLite2-City.mmdb. Defaults to `<data>/geoip/GeoLite2-City.mmdb` if unset.          |
| `cloudflare_api_token`   | string \| null    | Cloudflare API token with `Zone.Analytics:Read` for the zone.                                  |
| `cloudflare_zone_id`     | string \| null    | The zone ID matching `cloudflare_api_token`. Both must be set to enable the Cloudflare panels. |
| `secret_scan_paths`      | string[]          | Directory paths the secrets scanner walks recursively.                                         |
| `postgres_url`           | string \| null    | libpq URL (`postgresql://user:pass@host:port/db`) for the Postgres admin page.                 |
| `clamav_addr`            | string \| null    | TCP address of a `clamd` daemon, e.g. `"127.0.0.1:3310"`. Enables ClamAV scanning.             |
| `virustotal_api_key`     | string \| null    | VirusTotal public API key. Enables hash-based lookups on the File Sanitizer page.              |
| `spotlight_scripts`      | SpotlightScript[] | Pre-defined shell commands runnable from the Ctrl+K palette. See below.                        |
| `authorized_keys_path`   | string \| null    | Path of an `authorized_keys` file that's rewritten on every add/revoke/delete. See below.      |
| `sshd_auth_log_path`     | string \| null    | Path of the host sshd auth log to tail in order to populate each key's "Last used". See below. |

### Docker services configuration

Each entry powers one card on `/admin/docker`:

```json
{
  "name": "Percy",
  "identifier": "percy_main",
  "path": "/home/parzival/Percy"
}
```

| Field        | Description                                                                                                                                                                                 |
|--------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `name`       | Human-readable label shown on the card.                                                                                                                                                     |
| `identifier` | Container name passed to `docker ps` / `stop` / `start`.                                                                                                                                    |
| `path`       | (Optional) Path containing a `docker-compose.yml`. When set, Start/Stop/Restart/Pull/Recreate use `docker compose` commands in that directory instead of plain `docker start/stop/restart`. |

> **Note:** The legacy `kind` field (`"docker"` / `"screen"`) is silently ignored if present in an old config file. Screen support has been removed — all services are Docker.

### Spotlight scripts configuration

Pre-defined shell commands that appear in the Ctrl+K palette under the **Scripts** section and can be run directly from the palette:

```json
"spotlight_scripts": [
  {
    "id": "restart-nginx",
    "name": "Restart nginx",
    "command": "systemctl restart nginx",
    "description": "Reload nginx config and restart the service",
    "cwd": null
  },
  {
    "id": "git-pull-site",
    "name": "Pull site repo",
    "command": "git pull",
    "cwd": "/home/user/mysite"
  }
]
```

| Field         | Description                                                                                     |
|---------------|-------------------------------------------------------------------------------------------------|
| `id`          | Unique identifier used internally. Must be unique across all scripts.                           |
| `name`        | Display name shown in the palette.                                                              |
| `command`     | Shell command executed via `sh -c` on Unix or `cmd /C` on Windows.                              |
| `description` | (Optional) Subtitle shown below the name in the palette. Defaults to the raw command string.    |
| `cwd`         | (Optional) Working directory for the command. Defaults to the process working directory.        |

Scripts time out after 30 seconds. Every execution is recorded in the audit log (`spotlight.script.run` with the script name as target).

### Enabling the security dashboard's optional features

**GeoIP** — sign up for a free account at [maxmind.com](https://www.maxmind.com), download the GeoLite2-City **binary** database (the `.mmdb` file, not the CSV). Drop the file in any of these locations — the first existing one wins:

| Priority | Location                                                          |
|----------|-------------------------------------------------------------------|
| 1        | The exact path in `config.geoip_db_path` (if set).                |
| 2        | `<data dir>/geoip/GeoLite2-City.mmdb`                             |
| 3        | `<data dir>/GeoLite2-City.mmdb`                                   |
| 4        | `<config dir>/geoip/GeoLite2-City.mmdb`                           |
| 5        | `<config dir>/GeoLite2-City.mmdb`                                 |

On Windows the data dir and config dir are both `%AppData%\klappstuhl_me\`, so simply dropping the `.mmdb` file alongside your `config.json` works. For Docker that's `./data/geoip/GeoLite2-City.mmdb` on the host (mapped to `/data/geoip/` in the container). If no file is found, the security dashboard still works — country/city columns are simply hidden.

**Cloudflare** — create an API token with `Zone.Analytics:Read` on your zone, paste it as `cloudflare_api_token` along with the `cloudflare_zone_id`. The Cloudflare section appears automatically. Failures are non-fatal — if CF is unreachable, the rest of the dashboard still renders.

**ClamAV** — run `clamd` on the same host (or reachable via TCP) and set `clamav_addr` to its address. The File Sanitizer streams uploaded files to clamd using the native INSTREAM protocol — no `clamdscan` binary required.

**VirusTotal** — set `virustotal_api_key` to a free public API key. The File Sanitizer computes the SHA-256 of each upload and does a hash-only lookup against VT v3 — no file data is ever sent to VirusTotal. Files not yet in the VT database show as "Not in VT".

**Postgres** — set `postgres_url` to a libpq connection string. The configured credential should have at least `pg_read_all_data`; a superuser works too since safe-mode queries are always wrapped in `BEGIN TRANSACTION READ ONLY`.

### SSH key sync and "Last used" tracking

The `/admin/ssh` page stores keys in SQLite by default — sshd on the host doesn't know about them until you wire one or both of the integrations below. They're independent, so you can enable either, both, or neither.

**File sync — `authorized_keys_path`.** When set, every add / revoke / delete rewrites the file at that path (atomic temp-file + `rename`, mode `0600` on Unix). The parent directory must exist and be writable from inside the container. The export endpoint at `/admin/ssh/export/authorized_keys` is still available for one-shot downloads.

Recommended Docker layout — bind-mount the target user's `.ssh` dir into the container and point the config at it:

```yaml
volumes:
  - /home/parzival/.ssh:/host-ssh
```
```json
"authorized_keys_path": "/host-ssh/authorized_keys"
```

Replace `parzival` with `root` (and the path with `/root/.ssh`) if you SSH in as root. The mounted directory is rewritten in place — pre-existing `authorized_keys` content **is overwritten**, so move any hand-managed keys into the admin UI first or back the file up.

**"Last used" — `sshd_auth_log_path`.** When set, a background thread tails the file and updates `ssh_key.last_used_at` whenever sshd logs a successful publickey auth whose `SHA256:<fingerprint>` matches a stored key (also fires an `ssh.key.use` audit entry). Survives log rotation via inode + size checks and reopens on errors.

```yaml
volumes:
  - /var/log/auth.log:/host-log/auth.log:ro
```
```json
"sshd_auth_log_path": "/host-log/auth.log"
```

Distro-specific log locations:

| Distro                 | Log path                |
|------------------------|-------------------------|
| Debian / Ubuntu        | `/var/log/auth.log`     |
| RHEL / Fedora / Rocky  | `/var/log/secure`       |
| Alpine / Arch          | usually journald only — see below |

sshd must log fingerprints for the parser to match. On modern OpenSSH this is the default; if your `Accepted publickey ...` lines lack `ssh2: <algo> SHA256:<fp>`, set `LogLevel VERBOSE` in `/etc/ssh/sshd_config` and reload sshd.

If the host runs systemd-only (no rsyslog / no `/var/log/auth.log`), the simplest fix is `apt install rsyslog` (or the distro equivalent); the watcher needs a real file. Piping `journalctl -u ssh -f` into a file via a sidecar works too but is uglier.

Unknown / revoked fingerprints in the log are ignored at DEBUG level — a noisy log from brute-force attempts will not flood the audit table.

## Metric alert thresholds

Hard-coded (`src/metrics/alerts.rs`). On the `OK → ALERT` transition, a red Discord embed fires via `discord_webhook_url` with a 30-minute cooldown per metric:

| Metric          | Threshold | Notes                                  |
|-----------------|-----------|----------------------------------------|
| CPU             | > 90%     | Must be sustained for 5 minutes.       |
| RAM             | > 90%     | Instant.                               |
| Disk (root `/`) | > 90%     | Instant.                               |

## Data and log paths

When running directly (not in Docker), paths follow [XDG basedirs](https://specifications.freedesktop.org/basedir-spec/) on Linux:

| Kind          | Linux                                  | macOS                                                     | Windows                              |
|---------------|----------------------------------------|-----------------------------------------------------------|--------------------------------------|
| Config        | `$XDG_CONFIG_HOME/klappstuhl_me/`      | `~/Library/Application Support/klappstuhl_me/`            | `%AppData%\klappstuhl_me\`           |
| Database      | `$XDG_DATA_HOME/klappstuhl_me/`        | `~/Library/Application Support/klappstuhl_me/`            | `%AppData%\klappstuhl_me\`           |
| Logs          | `$XDG_STATE_HOME/klappstuhl_me/`       | `./logs/`                                                 | `./logs/`                            |
| ACME cache    | `$XDG_CACHE_HOME/klappstuhl_me/`       | `~/Library/Caches/klappstuhl_me/`                         | `%LocalAppData%\klappstuhl_me\`      |

Inside the Docker image these all live under `/data` via `XDG_CONFIG_HOME=/data/config`, `XDG_DATA_HOME=/data/data`, `XDG_STATE_HOME=/data/state`, `XDG_CACHE_HOME=/data/cache`.

## Building from source

Requires Rust 1.74 or higher (uses 2021 edition with some newer stdlib features).

```bash
cargo build --release
./target/release/klappstuhl_me           # run normally
./target/release/klappstuhl_me admin     # create the first admin account
```

The `static/` directory must be alongside the binary at runtime — it serves the CSS, JS, and image assets.

### Database migrations

The schema is versioned via `PRAGMA user_version` and migrations are applied automatically on startup from `sql/0.sql` through `sql/N.sql`. To add a migration, drop a new `sql/<N+1>.sql` ending with `PRAGMA user_version = <N+1>;` and bump the array length in `src/main.rs`.

The `request` table (in `requests.db`, separate from `main.db`) uses idempotent `ALTER TABLE` for compatibility with existing production databases.

## Project layout

```
src/
├── main.rs           — entry point: server, ACME, run/admin commands
├── lib.rs            — module roots
├── auth.rs           — Argon2 helpers
├── cloudflare.rs     — Cloudflare GraphQL Analytics client
├── config.rs         — Config struct + JSON load/save
├── database.rs       — async SQLite worker pool with prepared-stmt cache
├── docker.rs         — bollard Docker client wrapper + event watcher background task
├── geoip.rs          — Optional MaxMind reader wrapper
├── logging.rs        — Request log middleware + writer
├── metrics/          — Live metrics (host parsers, docker stats, alerts)
├── models.rs         — DB row types (Account, Session, Invite, ImageEntry)
├── routes/
│   ├── admin.rs      — Dashboard
│   ├── audit.rs      — Audit log
│   ├── auth.rs       — Login / signup / password change
│   ├── docker.rs     — Docker services + graph + snapshots + log SSE
│   ├── image.rs      — Image upload and gallery
│   ├── metrics.rs    — Metrics charts + stats data
│   ├── postgres.rs   — Postgres browser + query runner
│   ├── sanitizer.rs  — File sanitizer (ClamAV + VirusTotal)
│   ├── secrets.rs    — Secret scanner
│   ├── security.rs   — Security dashboard + GeoIP + Cloudflare
│   ├── spotlight.rs  — Ctrl+K search + script runner
│   ├── ssh.rs        — SSH key management + token/session audit
│   └── ws.rs         — WebSocket live-push hub
└── token.rs          — Session token signing + cookie helpers

templates/            — Askama HTML templates
static/               — CSS, JS, images served verbatim
sql/                  — Numbered migration files (0.sql … 8.sql)
```

## License

AGPL-3.0 — see [LICENSE](LICENSE).
