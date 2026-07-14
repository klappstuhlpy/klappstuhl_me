# Features

What the running app gives you: the admin control panel, the account shell, the
Discord/Percy integration, and where everything lands on disk.

- [Admin dashboard](#admin-dashboard)
- [Account](#account)
- [Discord login & the Percy dashboard](#discord-login--the-percy-dashboard)
- [Changelog & versioning](#changelog--versioning)
- [Data & log paths](#data--log-paths)

See also: [Setup](setup.md) for how to install and configure these, and the live
[API reference](https://klappstuhl.me/api/docs) for the public JSON API.

## Admin dashboard

`/admin` is a self-hosting control panel. Every integration is **optional and
non-fatal** — an unreachable or unconfigured backend simply hides its panel:

- **Metrics & security** — host/app metrics, request analytics, GeoIP + Cloudflare-backed security views.
- **File Sanitizer** — ClamAV (INSTREAM) + VirusTotal hash-lookup scanning of uploads.
- **Databases** — browse the internal SQLite files and (read-only) an external Postgres.
- **SSH keys** — store keys, sync to host `authorized_keys` via bind-mounts, track "last used" from the sshd log.
- **Firewall** — nftables/ufw/iptables rule manager with automatic brute-force lockouts.
- **Reverse proxy** — per-subdomain nginx/Caddy config generation, or Cloudflare Tunnel ingress via the CF API.
- **Docker** — start/stop/restart/pull service containers and detect image updates.
- **Backups** — scheduled `VACUUM INTO` snapshots with retention and optional S3 off-site mirroring.
- **Secrets scanner**, **audit log**, and a **Ctrl+K command palette** (with optional cron scripts).

Container image-update status is also exposed at `GET /api/v1/admin/updates` (scope `admin:read`).

## Account

`/account` is a sidebar shell rather than one long page — each area owns a route:

| Page                | What it does                                                                        |
|---------------------|-------------------------------------------------------------------------------------|
| `/account`          | Overview: stat tiles, a security checklist, recent account activity, quick actions  |
| `/account/profile`  | Identity, changing your username, and the Discord link                              |
| `/account/security` | Password, two-factor (TOTP) enrollment, recovery-code status, sign-in history       |
| `/account/sessions` | Active sign-ins — rename or revoke individually, or sign out everywhere             |
| `/account/api`      | The scoped API token, a `curl` starter, and the ShareX uploader config              |
| `/account/content`  | Images, short links, and pastes you own                                             |
| `/account/danger`   | Data export and permanent account deletion                                          |

**Changing your username** (`POST /account/username`) re-authenticates with your
password, then renames the account under a single transaction that also writes the
`username_change` history. A username is an identity here — it addresses your public
page (`/user/:name`) and labels your audit rows — so handing one over is guarded:
the name you release stays reserved for **you** for 30 days (a rename is undoable,
and nobody can snipe the name you just left), you may rename once every 30 days, and
service labels the audit log uses for non-account actors (`system`, `scheduler`,
`anonymous`, `percy-service`) can never be taken. Signup and the rename dialog both
call `GET /account/username/check?name=…`, which answers with the *same* rules, so
the live "available / taken" hint can't promise a name the submit would refuse.

**Data export** (`GET /account/export`) downloads a JSON file with your account
metadata, image/link entries, paste metadata, and session labels — never password
hashes, TOTP secrets, or tokens.

**Account deletion** (`POST /account/delete`) is immediate and permanent — there is
no soft-delete or grace period. It requires a browser session (never an API key),
the username typed back, the current password (Discord-only accounts must set one
first), and a TOTP code when 2FA is on — recovery codes are rejected here, since
they exist to restore access rather than destroy it. It is rate-limited, refuses to
delete the **last admin account**, and offers a choice to delete or orphan your
uploaded images. Sessions, API keys, recovery codes, the Discord link, short links,
and pastes go with the account; audit-log rows are retained with the actor unlinked.

## Discord login & the Percy dashboard

Users can **log in / sign up with Discord** (requests only the `identify` scope);
configure the OAuth2 app under `discord` in `config.json` and register the
`redirect_uri` in the [Discord Developer Portal](https://discord.com/developers/applications).

The Percy bot dashboard is its **own application**
([`klappstuhlpy/percy-dashboard`](https://github.com/klappstuhlpy/percy-dashboard)),
served on the `percy.<domain>` subdomain — this app only links to it via `/percy`.
Set the same **`sso_secret`** in both apps and a user logged in here with a linked
Discord account is signed straight into the dashboard via a short-lived signed
handoff (no shared DB or session store); leave it unset and `/percy` is a plain
redirect. Both apps share their design system, cookie crypto, and Percy API client
through the [`klappstuhl_me-shared`](https://github.com/klappstuhlpy/klappstuhl_me-shared) crates.

> **Deployment:** add `percy.<domain>` to `domains` so the ACME cert covers it
> (keep the apex first — it drives `canonical_url`, the cookie `Domain`, and the
> `r.` short-link subdomain). The session cookie is scoped to the registrable
> domain so the SSO handoff works across the subdomain.

## Changelog & versioning

The version in `Cargo.toml` is the single source of truth: it is shown in the
footer of every public page (linking to `/changelog`) and stamped into the
OpenAPI docs. Nothing else hardcodes a version string.

`/changelog` renders the repo-root `CHANGELOG.md` — the same file GitHub shows.
It is embedded into the binary at compile time and parsed into releases, each
rendered as a terminal window with colour-coded category badges (Added, Changed,
Deprecated, Removed, Fixed, Security). Entries under `## [Unreleased]` are never
published, so a change becomes public only when its release ships.

The format is a strict subset of [Keep a Changelog](https://keepachangelog.com):
releases are `## [X.Y.Z] - YYYY-MM-DD`, newest first, each holding only the six
category headings above with single-line `- ` bullets under them. `cargo test`
validates the real file against that grammar, so a malformed changelog fails the
build instead of breaking the page.

## Data & log paths

Running directly (not Docker), paths follow [XDG basedirs](https://specifications.freedesktop.org/basedir-spec/):

| Kind       | Linux                             | macOS                                          | Windows                         |
|------------|-----------------------------------|------------------------------------------------|---------------------------------|
| Config     | `$XDG_CONFIG_HOME/klappstuhl_me/` | `~/Library/Application Support/klappstuhl_me/` | `%AppData%\klappstuhl_me\`      |
| Database   | `$XDG_DATA_HOME/klappstuhl_me/`   | `~/Library/Application Support/klappstuhl_me/` | `%AppData%\klappstuhl_me\`      |
| Logs       | `$XDG_STATE_HOME/klappstuhl_me/`  | `./logs/`                                      | `./logs/`                       |
| ACME cache | `$XDG_CACHE_HOME/klappstuhl_me/`  | `~/Library/Caches/klappstuhl_me/`              | `%LocalAppData%\klappstuhl_me\` |

In Docker these live under `/data` via `XDG_*_HOME=/data/...`.
