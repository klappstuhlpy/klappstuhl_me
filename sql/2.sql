-- Revises: V1
-- Creation Date: 2026-05-25
-- Reason: Admin invite system — replace open registration with invite-only signup

CREATE TABLE IF NOT EXISTS invite
(
    code       TEXT    PRIMARY KEY,
    created_by INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TEXT,                 -- NULL = never expires
    used_at    TEXT,                 -- NULL = not yet used
    used_by    INTEGER REFERENCES account (id) ON DELETE SET NULL,
    note       TEXT                  -- free-form admin note (who it's for, etc.)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS invite_created_by_idx ON invite (created_by);
CREATE INDEX IF NOT EXISTS invite_used_idx       ON invite (used_at);

PRAGMA user_version = 2;
