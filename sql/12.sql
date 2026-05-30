-- TOTP two-factor authentication.

-- One row per recovery code; stored as a SHA-256 hash. used_at marks redemption.
CREATE TABLE IF NOT EXISTS totp_recovery_code (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    code_hash   TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    used_at     TEXT
);

CREATE INDEX IF NOT EXISTS totp_recovery_account_idx ON totp_recovery_code (account_id);
