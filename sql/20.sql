-- Converge `user_discord_links.discord_avatar` across every database.
--
-- The column was originally added by *editing* migration 13's CREATE TABLE
-- (commit 41b3dcd) instead of adding a new migration. Databases that applied
-- migration 13 before that edit — then got adopted into the checksum tracker at
-- the current checksum *without re-running the SQL* — therefore never got the
-- column. That breaks Discord account linking, because the link INSERT writes
-- `discord_avatar` and fails at runtime with "no such column".
--
-- SQLite has no `ADD COLUMN IF NOT EXISTS`, and a plain `ALTER TABLE ADD COLUMN`
-- would fail on the *other* databases — the fresh ones where migration 13 already
-- created the column. A table rebuild is the single operation that converges all
-- states deterministically. The copy lists only columns present in every version
-- of the table; the rebuilt table always has `discord_avatar` (NULL for existing
-- rows, which is re-fetched on the user's next login/link, so nothing of value is
-- lost). Nothing references `user_discord_links`, so the drop/rename is safe even
-- with `foreign_keys = ON`.

CREATE TABLE user_discord_links_new (
    account_id       INTEGER PRIMARY KEY REFERENCES account(id) ON DELETE CASCADE,
    discord_user_id  TEXT    NOT NULL UNIQUE,
    discord_username TEXT,
    linked_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    discord_avatar   TEXT
);

INSERT INTO user_discord_links_new (account_id, discord_user_id, discord_username, linked_at)
    SELECT account_id, discord_user_id, discord_username, linked_at
    FROM user_discord_links;

DROP TABLE user_discord_links;

ALTER TABLE user_discord_links_new RENAME TO user_discord_links;
