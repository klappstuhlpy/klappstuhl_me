-- Core schema: accounts, sessions, key-value storage, and audit log.
--
-- This is a clean reboot of the migration history. All prior migrations (0–27)
-- have been applied and consolidated into this fresh baseline.

CREATE TABLE IF NOT EXISTS account
(
    id           INTEGER PRIMARY KEY,
    name         TEXT UNIQUE NOT NULL,
    password     TEXT        NOT NULL,
    created_at   TEXT        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    flags        INTEGER     NOT NULL DEFAULT 0,
    totp_secret  TEXT,
    totp_enabled INTEGER     NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS account_name_idx ON account (name);


CREATE TABLE IF NOT EXISTS session
(
    id          TEXT PRIMARY KEY,
    account_id  INTEGER REFERENCES account (id) ON DELETE CASCADE,
    created_at  TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    description TEXT,
    api_key     INTEGER NOT NULL DEFAULT 0,
    scopes      TEXT    NOT NULL DEFAULT ''
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS session_account_id_idx ON session (account_id);
CREATE INDEX IF NOT EXISTS session_api_key_idx ON session (api_key);


CREATE TABLE IF NOT EXISTS storage
(
    name  TEXT PRIMARY KEY,
    value TEXT
) WITHOUT ROWID;


CREATE TABLE IF NOT EXISTS audit_log
(
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    actor_id     INTEGER REFERENCES account (id) ON DELETE SET NULL,
    actor_label  TEXT    NOT NULL,
    action       TEXT    NOT NULL,
    target       TEXT,
    ip           TEXT,
    meta_json    TEXT
);

CREATE INDEX IF NOT EXISTS audit_log_ts_idx     ON audit_log (ts);
CREATE INDEX IF NOT EXISTS audit_log_action_idx ON audit_log (action);
CREATE INDEX IF NOT EXISTS audit_log_actor_idx  ON audit_log (actor_id);
