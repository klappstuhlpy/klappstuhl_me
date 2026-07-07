-- Revises: V18
-- Creation Date: 2026-07-07
-- Reason: Hosted text/code pastes.
--   A lightweight paste bin exposed over the public API (pastes:read /
--   pastes:write scopes) and served for humans at /p/<id> (syntax-highlighted)
--   and /p/<id>.txt (raw). Pastes may optionally expire; an hourly reaper
--   deletes expired rows the same way the image reaper does. `id` is the same
--   random short id used for images/links so the URLs look consistent. `language`
--   is an optional syntect token/extension used to pick a highlighter.

CREATE TABLE IF NOT EXISTS paste
(
    id         TEXT PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    content    TEXT    NOT NULL,
    language   TEXT,
    views      INTEGER NOT NULL DEFAULT 0,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT
);

CREATE INDEX IF NOT EXISTS paste_account_idx ON paste (account_id);
CREATE INDEX IF NOT EXISTS paste_expires_idx ON paste (expires_at);
