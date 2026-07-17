# Changelog

All notable, user-visible changes to [klappstuhl.me](https://klappstuhl.me) are
documented in this file. It is also rendered at
[klappstuhl.me/changelog](https://klappstuhl.me/changelog).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
as interpreted for a website.

Entries for 1.1.0 through 1.4.2 were backfilled from commit history when this
changelog was introduced in July 2026; changes between 1.0.0 (January 2025) and
1.1.0 (July 2026) predate it and are unrecorded.

## [Unreleased]

### Changed

- Database migrations rebooted: the 28-file migration history (0–27) has been consolidated into 3 clean baseline files covering the same final schema — no tables, columns, or behavior changed.

## [1.7.0] - 2026-07-17

### Added

- A site traffic overview at **Account → Insights**: requests, active users, average response time and success rate over a chosen range, alongside the most-visited routes, referring sites, busiest API endpoints, and the accounts leaning hardest on the API.

### Removed

- **The admin control panel is no longer part of this site.** `/admin` and everything under it — host and container metrics, uptime monitoring, the security and audit views, Docker, the firewall, the reverse-proxy generator, SSH keys, backups, the database browser, the secrets scanner and the file sanitizer — now live in **Vantage**, a separate app on its own subdomain. The site is a personal website, image host and pastebin again; nothing about running the machine is served from it. The parts that were genuinely about the *site* rather than the host stayed: traffic analytics moved to Account → Insights (above), and login throttling is now the site's own (below).
- **Upgrading permanently deletes the old admin tables.** Metrics history, uptime history, firewall rules, proxy routes, stored SSH keys, secret findings and sanitizer results are dropped from the database on first start after this release, since nothing reads them here any more. Vantage keeps its own records and does not import these — if you still want anything out of them, take a copy of `main.db` **before** upgrading.
- **`config.json` has lost the host-administration keys**: `cloudflare`, `proxy`, `backup`, `services`, `geoip_db_path`, `postgres_url`, `secret_scan_paths`, `spotlight_scripts`, `sshd_auth_log_path`, `firewall_backend`, `update_check_interval_hours`, and the `ntfy`/webhook/email alert sinks. Configure those in Vantage instead. Nothing breaks if you leave them in the file — unknown keys are ignored on load and dropped the next time the config is re-normalised. `clamav_addr` and `virustotal_api_key` **stay**: the sanitizer *page* moved, but scanning uploads did not.
- **"Ask the AI" is gone.** The in-terminal assistant, the `POST /api/ask` proxy and the `ai` / `status_url` config keys have all been removed — the site no longer talks to any AI provider, and no API key is needed to run it. Its remaining use case went with the admin dashboard.
- **The Ctrl+K command palette is gone.** Its only entry point lived in the admin layout, so it has been unreachable since the admin panel left; with the assistant removed as well there was nothing left for it to do.
- The `admin:read`-scoped image-update endpoint has left the public API surface (it was operator-only and undocumented for general use); homelab image-update status lives in the standalone admin app.
- Removed unused cursor spotlight left-overs from previous styling version from index.js/index.css.

### Security

- Repeated failed logins from the same IP are now throttled by the site itself, independent of any firewall configuration.

## [1.6.0] - 2026-07-15

### Added

- A full pastebin you can use in the browser: create a paste at `/paste`, view it at `/p/<id>`, and manage your own at `/pastes` — no API key required.
- A rewritten paste viewer with syntax highlighting, clickable line numbers and range links (`#L12-L20`), a word-wrap toggle, copy-all, download, and a share panel with a QR code.
- Live syntax highlighting in the editor: you now type onto coloured syntax, not a plain textarea, with auto-indentation (Enter keeps the line's indent and adds a level after `:`/`{`, Backspace removes a whole step).
- A searchable language picker in the editor covering every language the highlighter knows, each with its brand logo, plus an **Auto-detect** default that infers the language from the paste's content (keywords and shebangs), filename, or first line.
- A **Format** button in the editor that pretty-prints JSON.
- Titles, visibility (public / unlisted / private), and one-click expiry presets (10 minutes to 30 days, or never) on every paste.
- Password-protected pastes: the body is encrypted at rest, and only someone with the password can read it.
- Burn-after-read pastes that are destroyed the first time they're revealed — with a confirmation step, so a link-preview in Discord or Slack can't destroy one before you click it.
- Anonymous pastes: `curl --data-binary @file.txt https://klappstuhl.me/p` returns a link (and a one-time edit token to manage it later). Can be turned off by the operator.
- Editing, forking, and full revision history with a per-edit diff for any paste.
- A secret scan on every paste: if it looks like it contains a live credential (an API key, a private key, a token) you're warned before publishing.
- Public pastes now appear on your `/user/<name>` profile, and are the only ones search engines are allowed to index.
- API: pastes gain `title`, `visibility`, `burn_after_read` and `password` on create; new `PATCH /api/pastes/{id}` (edit), `POST /api/pastes/{id}/fork`, and `GET /api/pastes/{id}/revisions` endpoints. Existing paste clients keep working unchanged.

### Changed

- The paste viewer now matches the rest of the site — terminal styling, the coral accent, and both light and dark themes — instead of the old bare dark page.

## [1.5.0] - 2026-07-14

### Added

- A public changelog at `/changelog`, rendered from this file with a colour-coded badge per category.
- The version the site is running now appears in the footer of every page, linking to the changelog.
- Profile pictures: your linked Discord avatar is shown on your account overview and on public profiles at `/user/<name>`. Accounts without a Discord link keep the default mark.

## [1.4.2] - 2026-07-14

### Changed

- Improved the password and API-key visibility toggles on the account pages.

## [1.4.1] - 2026-07-14

### Added

- Username changes: rename your account from the account settings, with a change history, a rename cooldown, a hold on the released name, and a live availability check on both signup and the rename dialog.

## [1.4.0] - 2026-07-14

### Added

- The account area is now a multi-page shell: profile, security, sessions, API keys, and a danger zone.
- Personal data export as JSON from the account area.
- Permanent account deletion; uploaded images you chose to keep and the audit trail survive ownerless.

## [1.3.3] - 2026-07-09

### Fixed

- Discord avatars are stored consistently after linking, and unlinking now asks for confirmation.

## [1.3.2] - 2026-07-09

### Fixed

- Logging out now also clears the Percy dashboard session.

## [1.3.1] - 2026-07-09

### Fixed

- The real Discord avatar is saved and served after linking a Discord account.

## [1.3.0] - 2026-07-08

### Added

- SVG chart rendering endpoints in the public API.
- Account introspection endpoint and color palette utilities in the public API.

### Changed

- Refreshed links across the site.

## [1.2.0] - 2026-07-07

### Added

- Paste hosting: upload and share text pastes through the site and the public API.

## [1.1.0] - 2026-07-07

### Added

- Per-guild gallery API keys.

### Security

- Privileged API scopes locked down: scope-limited keys now require an explicit grant.

## [1.0.0] - 2025-01-20

### Added

- Initial release: personal website, image hoster, and public JSON API.
