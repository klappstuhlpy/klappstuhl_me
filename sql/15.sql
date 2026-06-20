-- Discord guilds the user can manage, captured from the `/users/@me/guilds`
-- OAuth response at login/link time. Drives the dashboard's "Add Percy" section:
-- servers the user administrates but Percy is not yet a member of (Percy's
-- internal API can only see guilds the bot is already in).

CREATE TABLE IF NOT EXISTS user_discord_admin_guilds (
    discord_user_id TEXT    NOT NULL,
    guild_id        TEXT    NOT NULL,
    name            TEXT    NOT NULL,
    icon            TEXT,
    owner           INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (discord_user_id, guild_id)
);

CREATE INDEX IF NOT EXISTS user_discord_admin_guilds_user_idx
    ON user_discord_admin_guilds (discord_user_id);
