-- Revises: V16
-- Creation Date: 2026-07-06
-- Reason: Guild-scoped image galleries.
--   Images uploaded on behalf of a Discord guild carry the guild's snowflake so
--   the bot (poll banners) and the dashboard can list/manage one shared per-guild
--   gallery through the new /guilds/:id/images endpoints. A NULL guild_id is a
--   normal personal upload — existing rows and the web/API image flow are
--   unchanged.

ALTER TABLE images ADD COLUMN guild_id TEXT;

CREATE INDEX IF NOT EXISTS images_guild_idx ON images (guild_id);
