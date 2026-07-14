-- Username changes.
--
-- `account.name` is UNIQUE, which already stops two accounts holding the same
-- name at the same time. What it cannot express is *time*: a name that account A
-- gives up is instantly free for account B to take, and B would then inherit A's
-- public page (`/user/:name`) and every audit row A left behind under that label
-- (`audit_log.actor_label` records the name as it was written, and the admin
-- audit page filters by it).
--
-- This table is the missing history. It gives each rename two guarantees:
--
--   * a released name stays reserved for the account that released it for a
--     hold period, so a rename can be undone and nobody can snipe the name you
--     just left;
--   * an account can only rename once per cooldown period, so a name cannot be
--     churned to dodge the hold.
--
-- Rows are written in the same transaction as the `account.name` UPDATE (see
-- `site::account::username::claim`), so the history can never disagree with the
-- account row.

CREATE TABLE IF NOT EXISTS username_change
(
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    old_name   TEXT    NOT NULL,
    new_name   TEXT    NOT NULL,
    changed_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- "when did this account last rename?" (the cooldown check)
CREATE INDEX IF NOT EXISTS username_change_account_idx ON username_change (account_id, changed_at);

-- "who released this name, and when?" (the hold check, on every availability probe)
CREATE INDEX IF NOT EXISTS username_change_old_name_idx ON username_change (old_name, changed_at);
