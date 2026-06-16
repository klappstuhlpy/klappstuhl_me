-- Revises: V10
-- Creation Date: 2026-05-29
-- Reason: Reverse proxy / domain manager — subdomain routes mapping a
--         hostname to an upstream container/host:port, with optional SSL,
--         HTTP basic auth, rate limiting, and access rules.

-- One row per managed subdomain.  We mirror the route here so the UI can
-- list / edit / regenerate config on demand and audit who changed what.
-- The actual nginx (or Caddy) config file written to `proxy_config_dir`
-- is regenerated from these rows; this table is the source of truth.
CREATE TABLE IF NOT EXISTS proxy_route
(
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Fully-qualified hostname, e.g. "jellyfin.klappstuhl.me".
    subdomain        TEXT    NOT NULL UNIQUE,
    -- Upstream host the proxy forwards to (container name, IP, or DNS name).
    target_host      TEXT    NOT NULL,
    -- Upstream port.
    target_port      INTEGER NOT NULL,
    -- "http" | "https" — scheme used to reach the upstream.
    target_scheme    TEXT    NOT NULL DEFAULT 'http',
    -- Optional reference to a configured Docker service identifier so the
    -- UI can show "→ container: jellyfin".  NULL for raw host:port targets.
    container        TEXT,
    -- Whether the proxy should terminate TLS for this host (managed certs).
    ssl_managed      INTEGER NOT NULL DEFAULT 1,
    -- Whether traffic is fronted by Cloudflare (informational + tweaks the
    -- emitted config to trust CF real-IP headers).
    cloudflare_proxied INTEGER NOT NULL DEFAULT 0,
    -- Optional HTTP basic-auth gate.  When user is set, pass_hash holds an
    -- htpasswd-compatible (bcrypt/APR1) hash for the emitted config.
    http_auth_user   TEXT,
    http_auth_pass_hash TEXT,
    -- Optional requests-per-second cap applied at the proxy layer.
    rate_limit_rps   INTEGER,
    -- Free-form access rules as JSON: { "allow": ["10.0.0.0/8"], "deny": ["*"] }.
    access_rules_json TEXT,
    -- Extra directives appended verbatim into the server/site block.
    extra_config     TEXT,
    enabled          INTEGER NOT NULL DEFAULT 1,
    created_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS proxy_route_enabled_idx ON proxy_route (enabled);
