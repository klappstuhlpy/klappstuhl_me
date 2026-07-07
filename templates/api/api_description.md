### Overview

Welcome to the Klappstuhl.me API. You can use this API to access the contents of the site. The API is based off of a simple REST API with a few endpoints.

### Authentication

Klappstuhl.me uses API keys to allow access to the API. Authentication is done using the `Authorization` header. The key may be sent bare, or with a scheme prefix (`Bearer <key>`, `Key <key>`, or `Token <key>`) — all are accepted. Note that in order to use this API, an account is required. Please [register](/login) if you have not done so already.

If you have not generated an API key yet, you can do so on your [account page](/account). Keys are **scoped**: tick only the scopes an integration needs (`images:read`, `images:write`, `links:read`, `links:write`, `pastes:read`, `pastes:write`). A key with no scopes ticked has legacy full access.

### Errors

Errors are returned as JSON shaped after Discord's model: a human-readable
`message`, a machine-readable numeric `code`, and — for validation failures — an
`errors` object keyed by field (`{ "url": { "_errors": ["…"] } }`). The legacy
`error` field is still present as an alias of `message`.

### Versioning

The API is versioned by path prefix. All endpoints live under `{base}` — this
is the version new integrations should target, and every response carries an
`X-API-Version` header naming the version that served it. `GET /api` returns a
discovery document listing the available versions.

The bare, unversioned paths (`/api/scan`, `/api/convert`, …) still work as a
**deprecated alias** of the current version so existing keys and tools keep
functioning, but they respond with a `Deprecation` header and a `Link` pointing
at the successor. Please migrate to the `{base}` prefix.

### Endpoint groups

- **Images** — upload, delete, and bulk-download your hosted images.
- **Links** — a URL shortener: create (`POST {base}/links`), list
  (`GET {base}/links`), fetch (`GET {base}/links/{code}`), and delete
  (`DELETE {base}/links/{code}`) your short links. Requires `links:read` /
  `links:write`.
- **Pastes** — a text/code paste host: create (`POST {base}/pastes`), list
  (`GET {base}/pastes`), fetch (`GET {base}/pastes/{id}`), and delete
  (`DELETE {base}/pastes/{id}`). Bodies are also viewable, without auth, at
  `/p/{id}` (syntax-highlighted) and `/p/{id}.txt` (raw). Requires
  `pastes:read` / `pastes:write`.
- **Media** — apply visual effects (`{base}/image/{op}`), transcode between
  raster formats (`{base}/convert`), or inspect an image (`{base}/metadata`).
  Each accepts either a multipart `file` upload or a public image `url` that
  the server fetches on your behalf (private/reserved addresses are refused).
- **Render** — turn content into images/documents: a syntax-highlighted code
  screenshot (`{base}/render/code`, pure Rust), a QR code
  (`{base}/render/qr`, SVG or PNG), a web-page screenshot
  (`{base}/render/screenshot`), or Markdown → PDF (`{base}/render/markdown-pdf`).
  The latter two need a Chromium binary on the server and return `500
  (not available)` until one is installed. `{base}/convert/transcode` converts
  MOV→MP4 / HEIC→JPG via `ffmpeg` under the same arrangement.
- **Web** — unfurl a URL into Open Graph / link-preview metadata
  (`GET {base}/unfurl?url=`); the target is fetched SSRF-guarded.
- **Scan** — check an uploaded file for malware via ClamAV and VirusTotal
  (`{base}/scan`).

### Pagination

List endpoints (short links, pastes, guild galleries) use Discord-style cursor
pagination: pass `?limit=` (1–200, default 50) with `?before=`/`?after=` set to a
resource id. Results are newest-first, so `after` walks towards older items and
`before` towards newer ones.

### Internal endpoints (not for public use)

Some documented endpoints are **internal** and exist only for the operator's own
services (Percy's Discord bot and its dashboard). They are listed here purely for
reference — they are gated by scopes that are **never** granted to a normal
personal API key, so a key you generate on your account page cannot call them.
Please do not build against them:

- **Guild galleries** (`{base}/guilds/{guild_id}/images…`) — require the
  `images:guild` scope, used by Percy to manage per-Discord-guild image
  galleries. Keys are minted per guild by the service, not issued to users.
- **Admin** (`{base}/admin/…`) — require the `admin:read` / `admin:write` scopes
  and expose the operator's homelab state.

### Rate Limits

Rate limits are enforced at an IP level to prevent abuse and spam on the service. When a rate limit is hit, an HTTP 429 status code is returned with a Discord-shaped JSON body (`{ message, retry_after, global, code }`) and header information telling you how to proceed. A standard RFC 7231 `Retry-After` header (integer seconds) is sent alongside the `x-ratelimit-*` headers, plus `x-ratelimit-scope` (what the limit is keyed on) and `x-ratelimit-bucket` (which bucket, when several are layered).

#### Header Format

The following headers are returned when using a rate limited endpoint:

```
x-ratelimit-limit: 25
x-ratelimit-remaining: 14
x-ratelimit-reset: 1713373688
x-ratelimit-reset-after: 0.98
```
- **x-ratelimit-limit**: The number of requests that can be made.
- **x-ratelimit-remaining**: How many requests are left before hitting a 429.
- **x-ratelimit-reset**: The UNIX timestamp (seconds since midnight UTC on January 1st 1970) at which the rate limit resets. This can have a fractional component for milliseconds.
- **x-ratelimit-reset-after**: The total time in seconds to wait for the rate limit to restart. This can have a fractional component for milliseconds.