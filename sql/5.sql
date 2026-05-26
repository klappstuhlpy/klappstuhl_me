-- Revises: V4
-- Creation Date: 2026-05-26
-- Reason: Audit logs + API token scopes.
--   1. New audit_log table tracking every state-changing action.
--   2. New `scopes` column on `session` (idempotent — only takes effect
--      for new API keys; existing rows are treated as "legacy full
--      access" by the application).

CREATE TABLE IF NOT EXISTS audit_log
(
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- The actor performing the action.
    -- `actor_id` is NULL for anonymous (e.g. failed login attempts);
    -- `actor_label` is a human-readable fallback like the username at
    -- the time of the event or "anonymous" / "system".
    actor_id     INTEGER REFERENCES account (id) ON DELETE SET NULL,
    actor_label  TEXT    NOT NULL,
    -- Dot-namespaced action identifier (e.g. "auth.login.success",
    -- "service.start", "invite.create", "secret.dismiss").
    action       TEXT    NOT NULL,
    -- Free-form target string — invite code, service name, image id, etc.
    target       TEXT,
    -- Client IP at the time of the action (already de-proxied via
    -- real_client_ip() before being passed in).
    ip           TEXT,
    -- JSON blob with extra context — kept opaque from the schema's
    -- point of view but always a serde_json::Value at the app layer.
    meta_json    TEXT
);

CREATE INDEX IF NOT EXISTS audit_log_ts_idx     ON audit_log (ts);
CREATE INDEX IF NOT EXISTS audit_log_action_idx ON audit_log (action);
CREATE INDEX IF NOT EXISTS audit_log_actor_idx  ON audit_log (actor_id);

PRAGMA user_version = 5;
