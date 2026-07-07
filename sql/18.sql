-- Revises: V17
-- Creation Date: 2026-07-07
-- Reason: Per-guild image-gallery API keys.
--   Each Discord guild that uses the shared gallery gets its own
--   images:guild-scoped API key, minted on demand and owned by a dedicated,
--   non-personal service account. This means the bot never has to be handed a
--   personal/all-access key: it presents a narrow service token to provision
--   (get-or-create) a guild's key, then uses that per-guild key for that guild's
--   uploads/list/delete. Per-guild keys can be revoked independently.
--
--   The token string is itself a normal `session` row (api_key = 1, scopes =
--   'images:guild'); this table just maps a guild snowflake to that key so it can
--   be reused across restarts and revoked per guild. guild_id is TEXT to match
--   the images.guild_id column and to hold the full 64-bit snowflake safely.

CREATE TABLE IF NOT EXISTS guild_api_key
(
    guild_id   TEXT PRIMARY KEY,
    token      TEXT    NOT NULL,
    account_id INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS guild_api_key_token_idx ON guild_api_key (token);
