-- Discord OAuth2 identity linking.

CREATE TABLE IF NOT EXISTS user_discord_links (
    account_id       INTEGER PRIMARY KEY REFERENCES account(id) ON DELETE CASCADE,
    discord_user_id  TEXT    NOT NULL UNIQUE,
    discord_username TEXT,
    linked_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

PRAGMA user_version = 13;
