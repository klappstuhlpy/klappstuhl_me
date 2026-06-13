-- URL shortener: per-account short links served from the `r.` subdomain.

CREATE TABLE IF NOT EXISTS short_link (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    code        TEXT    NOT NULL UNIQUE,
    target_url  TEXT    NOT NULL,
    account_id  INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    clicks      INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX IF NOT EXISTS short_link_account_idx ON short_link (account_id);

PRAGMA user_version = 14;
