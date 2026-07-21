# Features

What the running app gives you: the account shell, the site insights, the
Discord/Percy integration, and where everything lands on disk.

- [Insights](#insights)
- [Account](#account)
- [Pastebin](#pastebin)
- [Discord login & the Percy dashboard](#discord-login--the-percy-dashboard)
- [Changelog & versioning](#changelog--versioning)
- [Data & log paths](#data--log-paths)

See also: [Setup](setup.md) for how to install and configure these, and the live
[API reference](https://klappstuhl.me/api/docs) for the public JSON API.

## Insights

`/account/insights` is a traffic overview for the site, visible to admin
accounts. It aggregates `requests.db` (the HTTP access log, kept for 45 days)
over a chosen range — today, 7, 14, or 30 days:

- **Tiles** — requests, active users, average response time, success rate.
- **Popular routes** — grouped by matched route pattern (`/p/:id`), not raw path.
- **Referring sites** — external referrers only, with the big search engines collapsed to a name.
- **API routes** and **top API consumers** — the busiest endpoints, and the accounts calling them, split by success and failure.

Aggregation happens in SQL; only counts reach the browser, never raw log rows.
`/static/*` is excluded throughout.

> **Host operations live elsewhere.** Server logs, uptime monitoring, metrics,
> the 4xx/security feed, Docker, firewall, proxy, backups and the secrets scanner
> are [Vantage](https://github.com/klappstuhlpy/vantage)'s job — a separate app on
> its own subdomain. This page is the part that is about the *site*: it needs the
> account database to name an API consumer, which Vantage deliberately cannot read.

## Account

`/account` is a sidebar shell rather than one long page — each area owns a route:

| Page                | What it does                                                                        |
|---------------------|-------------------------------------------------------------------------------------|
| `/account`          | Overview: stat tiles, a security checklist, recent account activity, quick actions  |
| `/account/profile`  | Identity, changing your username, and the Discord link                              |
| `/account/security` | Password, two-factor (TOTP) enrollment, recovery-code status, sign-in history       |
| `/account/sessions` | Active sign-ins — rename or revoke individually, or sign out everywhere             |
| `/account/api`      | The scoped API token, a `curl` starter, and the ShareX uploader config              |
| `/account/content`  | Images, short links, and pastes you own (links out to the [pastebin](#pastebin))    |
| `/account/danger`   | Data export and permanent account deletion                                          |
| `/account/insights` | Admin only: the site traffic overview (see [Insights](#insights))                   |

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

## Pastebin

A browser-first paste host. Create a paste at `/paste`, read it at `/p/<id>`,
manage your own at `/pastes`, and drive the whole thing over the JSON API under
`/api/pastes` (scopes `pastes:read` / `pastes:write`). The old raw view,
`/p/<id>.txt`, is unchanged and still the documented `raw_url`.

**The editor** (`/paste`) is a plain textarea with a line-number gutter — no
in-browser code editor to download. Drop a file onto it to load it (the extension
picks the highlighter), `Tab` inserts spaces, `Ctrl`/`Cmd`+`Enter` saves. You set a
title, a language, an expiry (10 minutes to 30 days, or never), and a visibility.

**Visibility** is `public`, `unlisted` (the default) or `private`. Every paste is
readable by anyone who has the link — that is what makes it linkable — so visibility
controls *listing and indexing*, not access: only `public` pastes are shown on your
`/user/<name>` profile and allowed into a search index. Real secrecy comes from the
two protections below. (There is deliberately no global "recent pastes" feed.)

**Password protection** encrypts the body at rest with Argon2id + ChaCha20-Poly1305.
No password is stored — the decryption succeeding *is* the check — so a lost password
means a lost paste. This is server-side encryption: unlike a zero-knowledge,
key-in-the-URL design, the operator could in principle read a paste if they held its
password. The trade keeps server-side highlighting, link-preview cards and the API
working. Unlocking sets a short-lived, paste-scoped cookie so the raw and embed views
work without re-prompting.

**Burn-after-read** destroys a paste the first time it is *explicitly* revealed. A
plain visit shows a confirmation screen instead of the body, so a link-preview
crawler (Discord, Slack, iMessage all prefetch URLs) can't destroy the paste before
its recipient clicks — only the reveal button does, and only ever once.

**Anonymous pastes** let a signed-out visitor (or `curl`) create one:

```sh
curl --data-binary @notes.txt https://klappstuhl.me/p
# → https://klappstuhl.me/p/ab12cd34
#   edit token: … (keep it — the only way to edit or delete this paste)
```

They carry a smaller size cap, a forced expiry, and a one-time **edit token** (the
only way to manage them afterwards, since there's no account). The whole anonymous
surface can be switched off with `paste.anonymous`.

**Every paste is scanned** on save: if the body looks like it contains a live
credential (an API key, a private key, a token), you're warned and can publish anyway
— except an anonymous paste, where a detected secret is refused outright.

**The viewer** highlights the code with clickable line anchors (`#L12`, and
`#L12-L20` ranges), a word-wrap toggle, a per-browser theme picker, copy-all,
download, an embeddable view (`/p/<id>/embed`), a link-preview image
(`/p/<id>/og.svg`), forking, and full revision history with a per-edit diff. Markdown
pastes get a rendered view — through a sanitising renderer that strips embedded HTML,
so a markdown paste can't run script.

Limits (paste count, total bytes, sizes, the anonymous switch and TTL) are all
configurable — see [Setup](setup.md#pastebin). Admins bypass the quotas.

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
through the [`kls-core`](https://github.com/klappstuhlpy/kls-core) crates.

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
