-- Auth addons and integrations: TOTP recovery, Discord linking, guild API keys,
-- and username change history.

CREATE TABLE IF NOT EXISTS totp_recovery_code
(
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    code_hash   TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    used_at     TEXT
);

CREATE INDEX IF NOT EXISTS totp_recovery_account_idx ON totp_recovery_code (account_id);


CREATE TABLE IF NOT EXISTS user_discord_links
(
    account_id       INTEGER PRIMARY KEY REFERENCES account (id) ON DELETE CASCADE,
    discord_user_id  TEXT    NOT NULL UNIQUE,
    discord_username TEXT,
    linked_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    discord_avatar   TEXT
);


CREATE TABLE IF NOT EXISTS guild_api_key
(
    guild_id   TEXT PRIMARY KEY,
    token      TEXT    NOT NULL,
    account_id INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS guild_api_key_token_idx ON guild_api_key (token);


CREATE TABLE IF NOT EXISTS username_change
(
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    old_name   TEXT    NOT NULL,
    new_name   TEXT    NOT NULL,
    changed_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS username_change_account_idx ON username_change (account_id, changed_at);
CREATE INDEX IF NOT EXISTS username_change_old_name_idx ON username_change (old_name, changed_at);
