-- Revises: V0
-- Creation Date: 2025-01-17
-- Reason: Initial

CREATE TABLE IF NOT EXISTS account
(
    id         INTEGER PRIMARY KEY,
    name       TEXT UNIQUE NOT NULL,
    password   TEXT        NOT NULL,
    created_at TEXT        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    flags      INTEGER     NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS account_name_idx ON account (name);


CREATE TABLE IF NOT EXISTS session
(
    id          TEXT PRIMARY KEY,
    account_id  INTEGER REFERENCES account (id) ON DELETE CASCADE,
    created_at  TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    description TEXT,
    api_key     INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS session_account_id_idx ON session (account_id);
CREATE INDEX IF NOT EXISTS session_api_key_idx ON session (api_key);