-- Vanity URLs for public leaderboard pages. First-come-first-serve; transferable
-- on request by updating the guild_id.

CREATE TABLE IF NOT EXISTS percy_leaderboard_vanity (
    slug     TEXT    NOT NULL PRIMARY KEY,
    guild_id TEXT    NOT NULL UNIQUE,
    claimed_by TEXT  NOT NULL,
    created_at TEXT  NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX IF NOT EXISTS percy_leaderboard_vanity_guild_idx
    ON percy_leaderboard_vanity (guild_id);
