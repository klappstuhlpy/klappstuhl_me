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
  - **Health** — internal uptime monitoring (a self-hosted Uptime-Kuma). Define targets of four kinds — **HTTP** (status-code / keyword assertions, redirect following), **TCP** (port reachability), **keyword** (substring present/absent in an HTTP body), and **SSL** (certificate expiry, with a configurable "warn N days before" threshold). A background monitor probes each target on its own interval, records latency, classifies each sample as up / degraded (slower than the per-target `degraded_ms` threshold) / down, and opens/closes incidents automatically. Per-target uptime %s (24h / 7d / 30d), average + p95 latency, an incident timeline, and a sample history sparkline. Run a probe on demand, and get a Discord webhook on down→up / up→down transitions. Old samples are pruned after 30 days.
  - **Security** — failed logins, top offending IPs (with GeoIP country/city), reason breakdown, country distribution, recent activity feed. Optional Cloudflare panels (zone analytics + WAF events) when an API token + zone ID are configured.
  - **Firewall** — visual frontend for **nftables**, **ufw**, or **iptables** (auto-detected at startup, overridable via `firewall_backend`). Create allow / deny / rate-limit / geo-block rules (source CIDR, port, protocol, country, requests-per-second); each rule is mirrored in SQLite and applied to the kernel by shelling out to the detected backend. On ufw hosts the dashboard also **imports the live ruleset** — `ufw status` is parsed and reconciled into the mirror on each load, so rules created out-of-band (`ufw allow OpenSSH` from a shell) appear automatically. Manual IP lockouts with optional expiry, plus **automatic lockout** — after 8 failed logins from an IP within 10 minutes the address is blocked for an hour (driven off the existing audit log) and the block is pushed to the backend. A background reaper releases expired lockouts. "Re-apply all" re-pushes every enabled rule. When no backend binary is present the page still manages rules in the DB (handy in dev / without `NET_ADMIN`).
  - **Proxy** — reverse-proxy / domain manager. Map a subdomain (`jellyfin.klappstuhl.me`) to an upstream container or `host:port`, pick the scheme, and toggle managed TLS, Cloudflare-proxied real-IP handling, HTTP basic auth (password hashed with bcrypt), a requests-per-second rate limit, and JSON allow/deny access rules. From the route list the server renders an nginx `server { … }` block (or a Caddyfile fragment, per `proxy_kind`) per enabled route, writes it to `proxy_config_dir/<subdomain>.conf` (plus an htpasswd sidecar for nginx auth), prunes stale managed files, and runs `proxy_reload_command`. Preview the generated config before saving. Container targets are populated from `config.services`. With no `proxy_config_dir` set, routes are still tracked in the DB as a record of which subdomain points where.
  - **Secrets** — periodic + on-demand filesystem scanner with 18 built-in rules (AWS / GitHub / Stripe / OpenAI / Anthropic / Discord / Slack tokens, PEM private keys, JWTs, DB URLs). Findings stored deduplicated with first/last-seen tracking; dismiss / resolve / reopen workflow. Discord webhook on new criticals.
  - **Audit log** — every state-changing action records actor, action, target, IP, and a JSON `meta` blob. Auth events (login success/fail, 2FA challenge/fail, signup, password change, logout), invite create/revoke, service/snapshot/script actions, secret status changes, backup create/delete, and admin cache invalidation are all tracked. Filterable by action prefix and actor.
  - **Logs** — interactive viewer over the rolling tracing log files. Parses the JSON application log (one object per line) and the compact bad-request log best-effort, with level/text filtering and tailing. (Separate from the dashboard's request-log panels.)
  - **Postgres** — browse databases, tables, and roles on a separate PostgreSQL server. Safe-mode query runner wraps every statement in `BEGIN TRANSACTION READ ONLY` so even a superuser credential can't accidentally mutate state; explicit danger-mode toggle for `INSERT` / `UPDATE` / `DELETE` / DDL when needed. Every query writes an audit entry (including blocked-by-safe-mode attempts).
  - **SSH Keys** — manage authorized SSH public keys stored in the database. Add keys with an optional label; optionally sync them to a real `authorized_keys` file on the host and populate each key's "Last used" timestamp by tailing the sshd auth log (see [SSH key sync](#ssh-key-sync-and-last-used-tracking)). Token audit (active API tokens across accounts) and session audit (active login sessions with IP + user-agent) sub-pages. Revoke individual keys, tokens, or sessions.
  - **File Sanitizer** — drag-and-drop file scanning with two optional backends.
    - **ClamAV** — streams the uploaded file to a local `clamd` daemon over TCP (INSTREAM protocol). Reports virus name on detection.
    - **VirusTotal** — looks up the file's SHA-256 against the VT v3 API (no file upload; hash-only). Shows `N/M engines detected` with a link to the VT report.
    - Scan history table (SQLite) with per-row deletion. Both backends are independent; results combine into a single clean/infected/unknown verdict.
  - **Backups** — on-disk SQLite backups via `VACUUM INTO` (a consistent, fully-checkpointed copy taken without an exclusive lock, safe while serving). A background scheduler takes one every `backup_interval_hours` and prunes to the most recent `backup_keep`. Take a backup on demand, download any backup file, or delete one. Restore is intentionally a manual, offline operation (stop the server, swap `main.db`) — under WAL it's unsafe to replace the live DB in place.
- **Two-factor auth (TOTP)** — opt-in RFC 6238 2FA managed from `/account`. Enroll by scanning a QR / entering the secret, confirm with a code, and download one-time recovery codes. On login, accounts with 2FA are bounced to `/login/2fa` (a short-lived signed pending cookie carries the challenge — never persisted) and must supply a TOTP code or a recovery code. The shared secret is encrypted at rest with ChaCha20-Poly1305 keyed by the app secret, so a leaked database (or a downloaded backup) doesn't expose usable 2FA secrets; recovery codes are stored only as SHA-256 hashes. Disabling 2FA requires a password-confirmation modal.
- **Public status page (`/status`)** — unauthenticated, derived from the Health monitors. Shows an overall banner (all operational / degraded / major outage), per-service up/degraded/down status, 24h uptime %, and last-check time.
- **Spotlight palette (Ctrl+K)** — macOS-style command palette available on every admin page. Opens with `Ctrl+K` (or the Search button in the sidebar footer). Fuzzy-searches across all admin nav items, configured scripts, audit log entries, file scan history, SSH keys, and live Docker containers. Keyboard-navigable (↑/↓/Enter/Esc). Scripts run inline and show their stdout/stderr output in the palette without leaving the page.
- **API tokens with scopes** — generated from `/account`, each token can be restricted to a subset of `images:read · images:write · admin:read · admin:write`, and every API endpoint enforces the scope it needs (image processing / render / scan all require `images:read`; upload/delete require `images:write`; `GET /api/admin/updates` requires `admin:read`). Legacy keys (created without selecting scopes) keep full access for back-compat.
- **Live updates over WebSocket** — `/ws` push topic events to dashboards. Metrics tiles refresh on every scrape, audit-log entries appear instantly, and Docker graph updates on container events; polling is the automatic fallback when the socket is closed.
- **Installable PWA** — `site.webmanifest` + service worker shell-cache the static assets. iOS standalone-mode meta tags + a black theme colour so the app looks native when installed to the home screen. Network-only for everything dynamic; offline access is intentionally **not** a goal for a homelab admin tool.
- **REST API** — OpenAPI 3.0 documented at `/api/docs` via utoipa + Scalar. All endpoints enforce token scopes (see below). Beyond image upload/download/delete:
  - **Media processing** — `POST /api/image/:op` (blur, pixelate, deepfry, invert, grayscale → PNG), `POST /api/convert` (transcode between raster formats), `POST /api/metadata` (image info). Each accepts a multipart `file` **or** a public `url`; URL fetches are SSRF-guarded (private/reserved addresses refused, redirects disabled, size-capped).
  - **Render** — `POST /api/render/code` renders syntax-highlighted code to an image (syntect; pick `language` + `theme`). `POST /api/render/screenshot` and `POST /api/render/markdown-pdf` drive a headless Chromium; `POST /api/convert/transcode` shells out to ffmpeg (video / HEIC). These three are config-gated and return an error when the backing binary isn't available.
  - **Scan** — `POST /api/scan` runs an uploaded file through ClamAV + VirusTotal and returns a combined report.
  - **Upload TTL** — `POST /api/images/upload?expires_in=<seconds>` auto-deletes the upload after the given time-to-live (capped at 365 days); omit for a permanent upload.
  - **Admin** — `GET /api/admin/updates` returns the per-service container image-update status (requires `admin:read`).
  - **Shareable links** — media/render endpoints accept `share=true` to store the result and return a JSON `ShareResult` with a short `/m/:id` link instead of raw bytes; `GET /m/:id` serves it back.
- **Alert fan-out** — metric, health, and secret alerts deliver to any of a Discord webhook, an [ntfy](https://ntfy.sh) topic, and a generic JSON webhook (`{title, level, body, fields}`), depending on which of `discord_webhook_url` / `ntfy_url` / `alert_webhook_url` are configured.
- **Automatic TLS** — Let's Encrypt via rustls-acme (TLS-ALPN-01) in production mode.

## Tech stack

- **Backend** — Rust, [Axum](https://github.com/tokio-rs/axum) 0.7, Tokio, SQLite (rusqlite, bundled).
- **Templates** — [Askama](https://github.com/djc/askama) (compile-time server-side rendering).
- **Storage** — two SQLite files in the data dir: `main.db` (accounts, sessions, images, invites, metrics, snapshots, SSH keys, file scans, health monitors, firewall rules, proxy routes, TOTP secrets/recovery codes), `requests.db` (HTTP access log). Periodic `VACUUM INTO` backups land in `<data>/backups/`.
- **Auth** — HMAC-signed session tokens, Argon2 password hashing, optional TOTP 2FA (RFC 6238; secret encrypted at rest with ChaCha20-Poly1305).
- **TLS** — Automatic Let's Encrypt via rustls-acme.
- **Metrics** — Native `/proc` and `/sys` parsing, `df` for disk usage, `docker stats` for containers. uPlot for charts.
- **GeoIP** — Offline [MaxMind GeoLite2-City](https://dev.maxmind.com/geoip/geolite2-free-geolocation-data) database via the `maxminddb` crate.
- **Cloudflare** — GraphQL Analytics API (zone traffic + firewall events) via `reqwest`.
- **Docker** — [bollard](https://github.com/fussybeaver/bollard) crate for the Docker daemon API (container list, inspect, events stream, `docker commit`). [Cytoscape.js](https://js.cytoscape.org/) for the dependency graph.
- **Virus scanning** — inline ClamAV INSTREAM TCP client; VirusTotal v3 REST API via `reqwest`. SHA-256 via the `sha2` crate.
- **Media** — the `image` crate for raster manipulation/conversion; [syntect](https://github.com/trishume/syntect) for code-to-image rendering; optional headless **Chromium** (screenshots, Markdown→PDF) and **ffmpeg** (video / HEIC transcode) invoked as external binaries.

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

| Mount                                       | Why                                                                                         |
|---------------------------------------------|---------------------------------------------------------------------------------------------|
| `./data:/data`                              | Persistent config, database, logs, ACME cert cache.                                         |
| `/var/run/docker.sock`                      | So `/admin/docker` can run `docker ps` / `docker compose up -d`.                            |
| `/proc:/host/proc:ro`                       | So `/admin/metrics` reports the **host's** CPU/RAM/network.                                 |
| `/sys:/host/sys:ro`                         | Same — for `/sys/block/*` (disk I/O counters).                                              |
| `/etc/ufw:/etc/ufw`                         | So the firewall dashboard reads and writes the **host's** ufw rule database.                |
| `/var/lib/ufw:/var/lib/ufw`                 | ufw runtime state (required alongside `/etc/ufw` for `ufw status` to show host rules).      |
| `/home:/host-home`, `/root:/host-root`      | (Optional) Host home roots, so the SSH admin page can write `authorized_keys` files.        |
| `/var/log/auth.log:/host-log/auth.log:ro`   | (Optional) Host sshd log, so the SSH admin page can populate `last_used_at`.                |

`HOST_PROC=/host/proc` and `HOST_SYS=/host/sys` are pre-set in the compose file so the metrics collector picks up the host filesystem.

The compose file uses **`network_mode: host`** so the container shares the host's network namespace. This is required for the firewall backend (`ufw`/`iptables`/`nftables`) to see and modify the real host packet-filter rules. As a side effect, port mappings are not used — the app binds directly to the host's ports (`:9510` in dev, `:443` in production). Services on `localhost` (e.g. ClamAV at `127.0.0.1:3310`) are reachable directly without the `host.docker.internal` alias.

Additionally, `/etc/ufw` and `/var/lib/ufw` are bind-mounted from the host. Host networking shares the kernel's iptables tables but `ufw status` reads its rule list from the filesystem (`/etc/ufw/user.rules`, `/etc/ufw/user6.rules`). Without these mounts the container would see its own empty ufw install instead of the host's configured rules. The mounts are read-write so that rules created through the UI are persisted back to the host's ufw database.

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
  "ntfy_url": null,
  "alert_webhook_url": null,
  "services": [],
  "geoip_db_path": null,
  "cloudflare_api_token": null,
  "cloudflare_zone_id": null,
  "secret_scan_paths": [],
  "postgres_url": null,
  "clamav_addr": null,
  "virustotal_api_key": null,
  "spotlight_scripts": [],
  "sshd_auth_log_path": null,
  "firewall_backend": null,
  "proxy_config_dir": null,
  "proxy_kind": null,
  "proxy_reload_command": null,
  "backup_interval_hours": null,
  "backup_keep": null,
  "backup_remote": null,
  "update_check_interval_hours": null,
  "chromium_path": null,
  "ffmpeg_path": null
}
```

| Key                      | Type              | Notes                                                                                                                                                                                             |
|--------------------------|-------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `production`             | bool              | `true` switches the server to TLS via Let's Encrypt on port 443.                                                                                                                                  |
| `domains`                | string[]          | Hostnames that ACME will request certificates for.                                                                                                                                                |
| `server.port`            | u16               | Listen port (`443` in production, anything else for dev).                                                                                                                                         |
| `server.ip`              | string            | The IP the server listens on (default `0.0.0.0`).                                                                                                                                                 |
| `secret_key`             | string            | Auto-generated; used for HMAC signing of session tokens and flash cookies.                                                                                                                        |
| `discord_webhook_url`    | string \| null    | Discord incoming webhook URL for metric / health / secret alerts.                                                                                                                                 |
| `ntfy_url`               | string \| null    | [ntfy](https://ntfy.sh) topic URL (e.g. `https://ntfy.sh/my-topic`). When set, alerts are also pushed here as plain text.                                                                         |
| `alert_webhook_url`      | string \| null    | Generic webhook URL. When set, alerts are also POSTed here as JSON `{title, level, body, fields}`.                                                                                                |
| `services`               | ServiceConfig[]   | Docker services shown on `/admin/docker`. See below.                                                                                                                                              |
| `geoip_db_path`          | string \| null    | Path to a GeoLite2-City.mmdb. Defaults to `<data>/geoip/GeoLite2-City.mmdb` if unset.                                                                                                             |
| `cloudflare_api_token`   | string \| null    | Cloudflare API token with `Zone.Analytics:Read` for the zone.                                                                                                                                     |
| `cloudflare_zone_id`     | string \| null    | The zone ID matching `cloudflare_api_token`. Both must be set to enable the Cloudflare panels.                                                                                                    |
| `secret_scan_paths`      | string[]          | Directory paths the secrets scanner walks recursively.                                                                                                                                            |
| `postgres_url`           | string \| null    | libpq URL (`postgresql://user:pass@host:port/db`) for the Postgres admin page.                                                                                                                    |
| `clamav_addr`            | string \| null    | TCP address of a `clamd` daemon, e.g. `"host.docker.internal:3310"`. Enables ClamAV scanning.                                                                                                     |
| `virustotal_api_key`     | string \| null    | VirusTotal public API key. Enables hash-based lookups on the File Sanitizer page.                                                                                                                 |
| `spotlight_scripts`      | SpotlightScript[] | Pre-defined shell commands runnable from the Ctrl+K palette. See below.                                                                                                                           |
| `sshd_auth_log_path`     | string \| null    | Path of the host sshd auth log to tail in order to populate each key's "Last used". See below.                                                                                                    |
| `firewall_backend`       | string \| null    | Force the firewall backend: `"nftables"`, `"ufw"`, `"iptables"`, or `"disabled"`. Unset = auto-detect by probing each binary. `"disabled"` keeps the UI but issues no kernel commands. See below. |
| `proxy_config_dir`       | string \| null    | Directory the `/admin/proxy` page writes generated config into (`<subdomain>.conf` for nginx, `<subdomain>.caddy` for Caddy). Unset = DB-only, nothing written to disk. See below.                |
| `proxy_kind`             | string \| null    | Config syntax to emit: `"nginx"` (default) or `"caddy"`.                                                                                                                                          |
| `proxy_reload_command`   | string \| null    | Shell command run after config is regenerated, e.g. `"nginx -s reload"` or `"systemctl reload nginx"`. Skipped when unset.                                                                        |
| `backup_interval_hours`  | u64 \| null       | Hours between automatic `VACUUM INTO` SQLite backups. `0` disables the scheduler; unset defaults to `24`. See below.                                                                              |
| `backup_keep`            | usize \| null     | Number of automatic backups to retain (older ones pruned). Unset defaults to `14`.                                                                                                                |
| `backup_remote`          | object \| null    | Off-site backup target. When set, each new backup is also uploaded to an S3-compatible store (B2 / R2 / AWS / MinIO). See [Off-site backups](#off-site-backups).                                  |
| `update_check_interval_hours` | u64 \| null  | Hours between background container image-update checks. `0` disables. Unset defaults to `12`. See [Container image updates](#container-image-update-detection).                                    |
| `chromium_path`          | string \| null    | Path to a Chromium/Chrome binary for the screenshot and Markdown→PDF render endpoints. Unset = probe common names on `PATH`; if none found those endpoints return an error.                       |
| `ffmpeg_path`            | string \| null    | Path to an `ffmpeg` binary for the video/HEIC transcode endpoint. Unset = use `ffmpeg` on `PATH`; if absent the endpoint returns an error.                                                        |

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

When the app runs in Docker and `clamd` runs on the host, there are three traps to avoid. Walk this checklist top-to-bottom and uploads should "just work":

1. **Make `clamd` listen on TCP, not just the unix socket.** In `/etc/clamav/clamd.conf` make sure both of these are uncommented:
   ```
   TCPSocket 3310
   TCPAddr 0.0.0.0
   ```
   Then `sudo systemctl restart clamav-daemon`. Verify with `sudo ss -tlnp | grep clamd` — you should see `LISTEN ... 0.0.0.0:3310`. If `systemd` co-owns the FD (you'll see `("systemd",pid=1,...)` next to `("clamd",pid=N,...)` in the `users:` column), that's fine — socket activation works as long as `clamd` itself is healthy.

2. **Let the container reach clamd.** Because the compose file uses `network_mode: host`, the container shares the host's network stack — `localhost` inside the container *is* the host. Set `clamav_addr` in `config.json` to:
   ```json
   "clamav_addr": "127.0.0.1:3310"
   ```
   No `host.docker.internal` alias or `extra_hosts` entry is needed.

3. **No extra UFW rule required for clamd.** With host networking the traffic never crosses a Docker bridge, so the old `ufw allow from 172.16.0.0/12 to any port 3310` rule is unnecessary (though harmless to keep).

Quick end-to-end test (install `nc` first if missing: `apt-get install -y --no-install-recommends netcat-openbsd`):

```bash
printf "zPING\0" | nc -w 3 127.0.0.1 3310
# expected output: PONG
```

Because the container uses host networking you can run this from either the host shell or `docker compose exec klappstuhl_me sh -c '...'` — both hit the same `127.0.0.1:3310`. If it hangs, check that clamd is listening (`ss -tlnp | grep 3310`) and that `TCPAddr 0.0.0.0` is set in `/etc/clamav/clamd.conf`.

**VirusTotal** — set `virustotal_api_key` to a free public API key. The File Sanitizer computes the SHA-256 of each upload and does a hash-only lookup against VT v3 — no file data is ever sent to VirusTotal. Files not yet in the VT database show as "Not in VT".

**Postgres** — set `postgres_url` to a libpq connection string. The configured credential should have at least `pg_read_all_data`; a superuser works too since safe-mode queries are always wrapped in `BEGIN TRANSACTION READ ONLY`.

### SSH key sync and "Last used" tracking

The `/admin/ssh` page stores keys in SQLite by default — sshd on the host doesn't know about them until you wire one or both of the integrations below. They're independent, so you can enable either, both, or neither.

**File sync — no config, just bind-mounts.** The target host user is derived from each key's comment (`root@laptop` → `root`, `parzival@nas` → `parzival`). The server writes to `/host-home/<user>/.ssh/authorized_keys` for normal users and `/host-root/.ssh/authorized_keys` for root, atomic temp-file + `rename`, mode `0600`. If `~/.ssh` doesn't exist on the host the server creates it (mode `0700`) and `chown`s both it and `authorized_keys` to match the home dir's UID/GID — that keeps sshd's `StrictModes yes` (the default) happy.

Recommended Docker layout — two bind-mounts of the host's home roots:

```yaml
volumes:
  - /home:/host-home
  - /root:/host-root
```

That's all. No config keys, no per-user wiring. To add a key for a new host user, just upload it — as long as that user exists on the host (i.e. `/home/<user>` is a directory), the add succeeds and the file appears on the next sync. If the user doesn't exist, the add returns HTTP 422 with a clear message.

Adding a key with no comment (`ssh-ed25519 AAAA…`) also returns 422 — the server has no way to know which account to route it to. Append `user@host` and retry.

The mounted directories are rewritten in place — pre-existing `authorized_keys` content **is overwritten** on the next sync, so move any hand-managed keys into the admin UI first or back them up. Legacy keys from before the `target_user` column was added show up as **not synced** in the admin UI; delete and re-add them to fix. The export endpoint at `/admin/ssh/export/authorized_keys` still works for one-shot downloads of every active key regardless of target.

**"Last used" — `sshd_auth_log_path`.** When set, a background thread tails the file and updates `ssh_key.last_used_at` whenever sshd logs a successful publickey auth whose `SHA256:<fingerprint>` matches a stored key (also fires an `ssh.key.use` audit entry). Survives log rotation via inode + size checks and reopens on errors.

```yaml
volumes:
  - /var/log/auth.log:/host-log/auth.log:ro
```
```json
"sshd_auth_log_path": "/host-log/auth.log"
```

Distro-specific log locations:

| Distro                 | Log path                          |
|------------------------|-----------------------------------|
| Debian / Ubuntu        | `/var/log/auth.log`               |
| RHEL / Fedora / Rocky  | `/var/log/secure`                 |
| Alpine / Arch          | usually journald only — see below |

sshd must log fingerprints for the parser to match. On modern OpenSSH this is the default; if your `Accepted publickey ...` lines lack `ssh2: <algo> SHA256:<fp>`, set `LogLevel VERBOSE` in `/etc/ssh/sshd_config` and reload sshd.

If the host runs systemd-only (no rsyslog / no `/var/log/auth.log`), the simplest fix is `apt install rsyslog` (or the distro equivalent); the watcher needs a real file. Piping `journalctl -u ssh -f` into a file via a sidecar works too but is uglier.

Unknown / revoked fingerprints in the log are ignored at DEBUG level — a noisy log from brute-force attempts will not flood the audit table.

### Firewall backend

`/admin/firewall` auto-detects a packet-filter backend at startup by probing
`nft`, then `ufw`, then `iptables` (the first one whose status command succeeds
wins). Set `firewall_backend` to pin a specific one, or `"disabled"` to keep the
UI working without touching the kernel — rule edits still persist to SQLite so
the page is usable in dev or in a container started without `--cap-add=NET_ADMIN`.

Applying rules and lockouts shells out to the backend, which needs root or
`CAP_NET_ADMIN`. If `sudo` is present the commands are prefixed with `sudo -n`
(non-interactive); otherwise the process must already be privileged. Each rule
create / toggle / delete and every lockout is recorded in the audit log
(`firewall.rule.*`, `firewall.lockout.*`, `firewall.apply_all`).

Automatic lockout reads the existing `auth.login.fail` audit entries: 8 failures
from one IP within 10 minutes triggers a 1-hour block (`firewall.lockout.auto`).
A background task releases expired lockouts every minute and removes the
corresponding kernel rule. nftables lockouts assume an `inet filter` table with
an `input` chain.

**Live ruleset import (ufw).** The `firewall_rule` table is normally a one-way
mirror — the UI writes rows and they're pushed to the kernel — which means a host
configured out-of-band shows an empty dashboard even though rules are live. To
close that gap, each time `/admin/firewall` loads its data the server runs
`ufw status`, parses it, and reconciles the result into the mirror: live rules
not yet present are inserted (tagged `{"source":"ufw"}` in their `meta_json`),
and previously-imported rows that no longer appear live are pruned.
Hand-made rows created in the UI carry no marker and are never touched. This is
ufw-only for now (its status output is stable and parsable; nft/iptables have no
equivalent import). A few accepted caveats: a UI-created rule that's also live in
ufw can appear twice (once as the UI row, once as an imported one); imported rows
are a read-only reflection, so deleting or toggling one in the UI may not map
cleanly to a `ufw delete` and it will simply re-import on the next load. The
import is best-effort — any failure (no backend, command error, parse miss) is
logged and swallowed so the dashboard still renders.

### Reverse proxy / domain manager

`/admin/proxy` keeps a row per managed subdomain in SQLite and (optionally)
renders real proxy config from it. Set `proxy_config_dir` to the directory your
proxy includes route files from, `proxy_kind` to `"nginx"` (default) or
`"caddy"`, and `proxy_reload_command` to whatever reloads the proxy:

```json
"proxy_config_dir": "/etc/nginx/conf.d",
"proxy_kind": "nginx",
"proxy_reload_command": "nginx -s reload"
```

On any route change (and via the "Regenerate & reload" button) the server writes
one file per enabled route — `<subdomain>.conf` for nginx, `<subdomain>.caddy`
for Caddy — prunes managed files that no longer map to an enabled route (only
files carrying the `# Managed by klappstuhl.me` marker are removed, so
hand-written config sharing the directory is left alone), then runs the reload
command. For nginx routes with HTTP basic auth an `<subdomain>.htpasswd` sidecar
is written alongside and referenced via `auth_basic_user_file`; Caddy embeds the
bcrypt hash inline. Use the per-route **Preview** to see the exact output before
it touches disk.

The emitted nginx config references conventional certbot cert paths
(`/etc/letsencrypt/live/<subdomain>/…`) and, for rate limits, a `limit_req_zone`
you must declare once in the `http {}` block (the per-route `limit_req` line and
a commented template are emitted for you). Anything in the route's **Extra
config** field is appended verbatim inside the server/site block. When
`proxy_config_dir` is unset the page is purely a record-keeping view — no files
are written. All changes are audited (`proxy.route.*`, `proxy.apply`).

### SQLite backups

A background task takes a `VACUUM INTO` snapshot of `main.db` every
`backup_interval_hours` (default 24; `0` disables the scheduler) and prunes to
the most recent `backup_keep` files (default 14). Backups land in
`<data>/backups/` as `backup-<unix-ts>.db`. `VACUUM INTO` produces a consistent,
fully-checkpointed copy without an exclusive lock, so it's safe while the server
is serving requests.

`/admin/backups` lists every backup with its size, takes one on demand, and lets
you download or delete individual files. **Restore is a manual, offline step:**
stop the server and replace `main.db` with the downloaded file — swapping it
under live WAL connections is unsafe, so the UI deliberately doesn't offer it.

### Off-site backups

Local backups share a disk with the live database, so a dead disk takes them
with it. Set `backup_remote` to also mirror every new backup to an
S3-compatible object store. Path-style addressing + AWS Signature V4 are used,
which AWS S3, Backblaze B2, Cloudflare R2, MinIO, and Wasabi all accept — no
extra binary or SDK:

```json
"backup_remote": {
  "kind": "s3",
  "endpoint": "https://s3.us-west-002.backblazeb2.com",
  "region": "us-west-002",
  "bucket": "my-backups",
  "prefix": "klappstuhl/",
  "access_key_id": "…",
  "secret_access_key": "…"
}
```

| Field               | Notes                                                                                                   |
|---------------------|---------------------------------------------------------------------------------------------------------|
| `kind`              | Storage backend. Currently only `"s3"`.                                                                 |
| `endpoint`          | Base URL of the store. For AWS use `https://s3.<region>.amazonaws.com`; for B2/R2/MinIO use their host. |
| `region`            | Signing region. AWS needs the real region; B2/R2/MinIO accept any value (defaults to `us-east-1`).      |
| `bucket`            | Destination bucket.                                                                                      |
| `prefix`            | (Optional) key prefix inside the bucket; a trailing slash is added automatically.                       |
| `access_key_id`     | Access key id.                                                                                           |
| `secret_access_key` | Secret access key.                                                                                       |

Each scheduled and manual backup is uploaded in the background; an upload
failure raises an alert through the configured sinks (Discord / ntfy / webhook)
so a silently broken target doesn't go unnoticed. The `/admin/backups` page
shows the configured target and adds a per-backup **Upload off-site** button
(handy to validate credentials right after configuring). Uploads are audited as
`backup.upload`.

### Container image update detection

For every entry in `config.services`, a background task asks the image's
registry what digest it currently serves for the running tag (Docker Registry
v2 API, with anonymous pull-token auth handled for Docker Hub / GHCR) and
compares it against the digest the local image was pulled at. A mismatch means
`docker pull` would fetch something newer. Results drive an **update available**
badge on the `/admin/docker` service cards; a **Check updates** button there
runs the check on demand. A freshly-discovered update (one that wasn't available
on the previous run) fans out an alert. Set `update_check_interval_hours` to
tune the cadence (default 12; `0` disables). The status is also exposed at
`GET /api/admin/updates` for external dashboards — the first endpoint to require
the `admin:read` token scope.

Images built locally (no registry digest) and private registries that need
credentials degrade gracefully to an `unknown` state rather than erroring.

### External render tools (Chromium / ffmpeg)

The `/api/render/screenshot`, `/api/render/markdown-pdf`, and
`/api/convert/transcode` endpoints shell out to external binaries:

- **Chromium** (screenshots + Markdown→PDF) — set `chromium_path` to a
  Chromium/Chrome binary, or leave it unset to probe common names on `PATH`.
- **ffmpeg** (video / HEIC transcode) — set `ffmpeg_path`, or leave it unset to
  use `ffmpeg` on `PATH`.

When the required binary can't be found the corresponding endpoint returns an
error rather than failing obscurely; the rest of the API is unaffected. The
code-render endpoint (`/api/render/code`) needs no external tool — it uses the
in-process `syntect` highlighter.

## Metric alert thresholds

Hard-coded (`src/services/metrics/alerts.rs`). On the `OK → ALERT` transition, an alert fans out to every configured channel (`discord_webhook_url` / `ntfy_url` / `alert_webhook_url`) with a 30-minute cooldown per metric:

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

The schema is versioned via `PRAGMA user_version` and migrations are applied automatically on startup from `sql/0.sql` through `sql/N.sql` (currently up to `sql/12.sql`). To add a migration, drop a new `sql/<N+1>.sql` ending with `PRAGMA user_version = <N+1>;` and bump the array length in `src/main.rs`.

The `request` table (in `requests.db`, separate from `main.db`) uses idempotent `ALTER TABLE` for compatibility with existing production databases.

## Project layout

The source is grouped by domain. `lib.rs` re-exports the leaf modules flat, so
internal paths stay short (`crate::config`, `crate::docker`, …) regardless of
the physical nesting.

```
src/
├── main.rs               — entry point: server, ACME, run/admin commands
├── lib.rs                — module roots + flat re-exports
├── core/                 — config, async SQLite pool, models, CLI, logging, app state, errors, utils
├── auth/                 — Argon2 helpers, session-token signing, secret key, TOTP 2FA
├── integrations/         — external services: Cloudflare, Discord/ntfy/webhook alerts, GeoIP, Chromium/ffmpeg helpers
├── media/                — image manipulation/conversion, metadata, code-to-image, shared SSRF-guarded fetch (scan.rs)
├── services/             — background/admin domains:
│   ├── docker.rs         — bollard client wrapper + event watcher
│   ├── backup/           — VACUUM INTO backups + scheduler (mod.rs) and S3 off-site upload (s3.rs)
│   ├── updates.rs        — container image-update detection (registry digest checks)
│   ├── cron.rs           — 5-field cron parser + scheduler for spotlight scripts
│   ├── alerts.rs         — alert fan-out
│   ├── audit.rs          — audit-log writer
│   ├── ssh.rs            — SSH key storage + log tailer
│   ├── firewall/         — backend abstraction (nft/ufw/iptables), storage, auto-lockout, ufw import
│   ├── health/           — uptime checker probes, storage, background monitor
│   ├── metrics/          — host parsers, docker stats, alerts, storage
│   ├── postgres/         — Postgres client + safe-mode wrapper
│   ├── proxy/            — route storage, nginx/caddy renderer, reload
│   └── secrets/          — rules, scanner, storage
└── web/                  — HTTP layer: caching, flash, headers, rate limiter
    └── routes/
        ├── admin.rs      — Dashboard
        ├── auth.rs       — Login / signup / 2FA / password change / account
        ├── audit.rs · backups.rs · logs.rs · docker.rs · firewall.rs · health.rs
        ├── image.rs · metrics.rs · postgres.rs · proxy.rs · sanitizer.rs
        ├── secrets.rs · security.rs · spotlight.rs · ssh.rs · ws.rs
        └── api/           — REST API: images, media, scan, code, external (Chromium/ffmpeg), auth/scopes

templates/                — Askama HTML templates (grouped into subfolders)
static/                   — CSS, JS, images served verbatim (grouped into subfolders)
sql/                      — Numbered migration files (0.sql … 12.sql)
```

## License

AGPL-3.0 — see [LICENSE](LICENSE).
