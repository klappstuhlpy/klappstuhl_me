# Klappstuhl.me

Personal website, image-hosting service, and homelab admin platform built with Rust.

What started as a small image host has grown into a single binary that also runs
the admin tooling for the machine it sits on: live system metrics, container
management, security analytics, and an invite-only user system.

## Highlights

- **Public site** — landing page, projects page, image gallery.
- **Image host** — drag-and-drop upload, public direct links, per-user library.
- **Admin shell at `/admin`** — sidebar layout with the following pages:
  - **Dashboard** — request analytics, popular routes, referring sites, API consumers.
  - **Invites** — invite-only signup. Admins generate one-time codes (with optional expiry + note), copy a `/signup?code=…` URL, share it with the invitee. No public registration.
  - **Services** — start / stop / restart Docker containers (or `docker compose` stacks via a configured `path`). Per-service live log console streamed over Server-Sent Events with syntax highlighting.
  - **Metrics** — live host stats (CPU, RAM, disk usage, disk I/O, network throughput) plus per-container Docker stats. uPlot charts with selectable ranges (1h / 6h / 24h / 7d / 30d). Threshold alerts fire a Discord webhook on `OK → ALERT` transitions with a 30-minute cooldown.
  - **Security** — failed logins, top offending IPs (with GeoIP country/city), reason breakdown, country distribution, recent activity feed. Optional Cloudflare panels (zone analytics + WAF events) when an API token + zone ID are configured.
  - **Secrets** — periodic + on-demand filesystem scanner with 18 built-in rules (AWS / GitHub / Stripe / OpenAI / Anthropic / Discord / Slack tokens, PEM private keys, JWTs, DB URLs). Findings stored deduplicated with first/last-seen tracking; dismiss / resolve / reopen workflow. Discord webhook on new criticals.
  - **Audit log** — every state-changing action records actor, action, target, IP, and a JSON `meta` blob. Auth events (login success/fail, signup, password change, logout), invite create/revoke, service actions, secret status changes, and admin cache invalidation are all tracked. Filterable by action prefix and actor.
  - **Postgres** — browse databases, tables, and roles on a separate PostgreSQL server (the one on your host, not the app's own SQLite). Safe-mode query runner wraps every statement in `BEGIN TRANSACTION READ ONLY` so even a superuser credential can't accidentally mutate state; explicit danger-mode toggle for `INSERT` / `UPDATE` / `DELETE` / DDL when needed. Every query writes an audit entry (including blocked-by-safe-mode attempts).
- **API tokens with scopes** — generated from `/account`, each token can be restricted to a subset of `images:read · images:write · admin:read · admin:write`. Legacy keys (created without selecting scopes) keep full access for back-compat.
- **Live updates over WebSocket** — `/ws` push topic events to dashboards. Metrics tiles refresh on every scrape and audit-log entries appear instantly; polling is the automatic fallback when the socket is closed.
- **Installable PWA** — `site.webmanifest` + service worker shell-cache the static assets. iOS standalone-mode meta tags + a black theme colour so the app looks native when installed to the home screen. Network-only for everything dynamic; offline access is intentionally **not** a goal for a homelab admin tool.
- **REST API** — OpenAPI 3.0 documented at `/api/docs` via utoipa + Scalar.
- **Automatic TLS** — Let's Encrypt via rustls-acme (TLS-ALPN-01) in production mode.

## Tech stack

- **Backend** — Rust, [Axum](https://github.com/tokio-rs/axum) 0.7, Tokio, SQLite (rusqlite, bundled).
- **Templates** — [Askama](https://github.com/djc/askama) (compile-time server-side rendering).
- **Storage** — three SQLite files inside the data dir: `main.db` (accounts, sessions, images, invites, metrics), `requests.db` (HTTP access log).
- **Auth** — HMAC-signed session tokens, Argon2 password hashing.
- **TLS** — Automatic Let's Encrypt via rustls-acme.
- **Metrics** — Native `/proc` and `/sys` parsing, `df` for disk usage, `docker stats` for containers. uPlot for charts.
- **GeoIP** — Offline [MaxMind GeoLite2-City](https://dev.maxmind.com/geoip/geolite2-free-geolocation-data) database via the `maxminddb` crate.
- **Cloudflare** — GraphQL Analytics API (zone traffic + firewall events) via `reqwest`.

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

| Mount                        | Why                                                            |
|------------------------------|----------------------------------------------------------------|
| `./data:/data`               | Persistent config, database, logs, ACME cert cache.            |
| `/var/run/docker.sock`       | So `/admin/services` can run `docker ps` / `docker compose up -d`.   |
| `/proc:/host/proc:ro`        | So `/admin/metrics` reports the **host's** CPU/RAM/network.    |
| `/sys:/host/sys:ro`          | Same — for `/sys/block/*` (disk I/O counters).                 |

`HOST_PROC=/host/proc` and `HOST_SYS=/host/sys` are pre-set in the compose file so the metrics collector picks up the host filesystem.

## Configuration

A default `config.json` is written on first start. Minimum production layout:

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
  "secret_scan_paths": []
}
```

| Key                    | Type            | Notes                                                                                          |
|------------------------|-----------------|------------------------------------------------------------------------------------------------|
| `production`           | bool            | `true` switches the server to TLS via Let's Encrypt on port 443.                               |
| `domains`              | string[]        | Hostnames that ACME will request certificates for.                                             |
| `server.port`          | u16             | Listen port (`443` in production, anything else for dev).                                      |
| `server.ip`            | string          | The ip the server runs on (default is localhost).                                              |
| `secret_key`           | string          | Auto-generated; used for HMAC signing of session tokens and flash cookies.                     |
| `discord_webhook_url`  | string \| null  | Used by metric threshold alerts.                                                               |
| `services`             | ServiceConfig[] | See "Services configuration" below.                                                            |
| `geoip_db_path`        | string \| null  | Path to a GeoLite2-City.mmdb. Defaults to `<data>/geoip/GeoLite2-City.mmdb` if unset.          |
| `cloudflare_api_token` | string \| null  | Cloudflare API token with `Zone.Analytics:Read` for the zone.                                  |
| `cloudflare_zone_id`   | string \| null  | The zone ID matching `cloudflare_api_token`. Both must be set to enable the Cloudflare panels. |
| `secret_scan_paths`    | string[]        | The list of paths that should be monitored with the secrets checcker                           |

### Services configuration

Each entry powers one card on `/admin/services`:

```json
{
  "name": "Percy",
  "kind": "docker",
  "identifier": "percy_main",
  "path": "/home/parzival/Percy"
}
```

| Field        | Description                                                                                       |
|--------------|---------------------------------------------------------------------------------------------------|
| `name`       | Human-readable label shown on the card.                                                           |
| `kind`       | `"docker"` (container) or `"screen"` (GNU Screen session).                                        |
| `identifier` | Container name passed to `docker ps`/`stop`/`start`, or the screen session name.                  |
| `path`       | (Docker only, optional) Path containing a `docker-compose.yml`. When set, Start/Stop/Restart use `docker compose up -d` / `down` / `restart` in this directory instead of plain `docker start/stop/restart`. |

### Enabling the security dashboard's optional features

**GeoIP** — sign up for a free account at [maxmind.com](https://www.maxmind.com), download the GeoLite2-City **binary** database (the `.mmdb` file, not the CSV). Drop the file in any of these locations — the first existing one wins:

| Priority | Location                                                          |
|----------|-------------------------------------------------------------------|
| 1        | The exact path in `config.geoip_db_path` (if set).                |
| 2        | `<data dir>/geoip/GeoLite2-City.mmdb`                              |
| 3        | `<data dir>/GeoLite2-City.mmdb`                                    |
| 4        | `<config dir>/geoip/GeoLite2-City.mmdb`                            |
| 5        | `<config dir>/GeoLite2-City.mmdb`                                  |

On Windows the data dir and config dir are both `%AppData%\klappstuhl_me\`, so simply dropping the `.mmdb` file alongside your `config.json` works. For Docker that's `./data/geoip/GeoLite2-City.mmdb` on the host (mapped to `/data/geoip/` in the container). If no file is found, the security dashboard still works — country/city columns are simply hidden, and startup logs list every path that was checked.

**Cloudflare** — create an API token with `Zone.Analytics:Read` on your zone, paste it as `cloudflare_api_token` along with the `cloudflare_zone_id`. The Cloudflare section appears automatically. Failures are non-fatal — if CF is unreachable, the rest of the dashboard still renders.

## Metric alert thresholds

Hard-coded (`src/metrics/alerts.rs`). On the `OK → ALERT` transition, a red Discord embed fires via `discord_webhook_url` with a 30-minute cooldown per metric:

| Metric          | Threshold | Notes                                  |
|-----------------|-----------|----------------------------------------|
| CPU             | > 90%     | Must be sustained for 5 minutes.       |
| RAM             | > 90%     | Instant.                               |
| Disk (root `/`) | > 90%     | Instant.                               |

## Data and log paths

When running directly (not in Docker), paths follow [XDG basedirs](https://specifications.freedesktop.org/basedir-spec/) on Linux:

| Kind          | Linux                                  | macOS                                                      | Windows                              |
|---------------|----------------------------------------|------------------------------------------------------------|--------------------------------------|
| Config        | `$XDG_CONFIG_HOME/klappstuhl_me/`     | `~/Library/Application Support/klappstuhl_me/`            | `%AppData%\klappstuhl_me\`           |
| Database      | `$XDG_DATA_HOME/klappstuhl_me/`       | `~/Library/Application Support/klappstuhl_me/`            | `%AppData%\klappstuhl_me\`           |
| Logs          | `$XDG_STATE_HOME/klappstuhl_me/`      | `./logs/`                                                  | `./logs/`                            |
| ACME cache    | `$XDG_CACHE_HOME/klappstuhl_me/`      | `~/Library/Caches/klappstuhl_me/`                          | `%LocalAppData%\klappstuhl_me\`      |

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
├── geoip.rs          — Optional MaxMind reader wrapper
├── logging.rs        — Request log middleware + writer
├── metrics/          — Live metrics (host parsers, docker stats, alerts)
├── models.rs         — DB row types (Account, Session, Invite, ImageEntry)
├── routes/           — HTTP handlers grouped by area
└── token.rs          — Session token signing + cookie helpers

templates/            — Askama HTML templates
static/               — CSS, JS, images served verbatim
sql/                  — Numbered migration files
```

## License

AGPL-3.0 — see [LICENSE](LICENSE).
