-- Revises: V5
-- Creation Date: 2026-05-26
-- Reason: SSH key management — authorized keys, temporary tokens, session audit.

CREATE TABLE IF NOT EXISTS ssh_key
(
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id   INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    name         TEXT    NOT NULL,
    -- Full OpenSSH public key string (e.g. "ssh-ed25519 AAAA... comment")
    public_key   TEXT    NOT NULL,
    -- SHA256 fingerprint in the form "SHA256:<base64>" (matches ssh-keygen -lf)
    fingerprint  TEXT    NOT NULL,
    -- Key algorithm: "ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256", etc.
    algo         TEXT    NOT NULL,
    -- Optional trailing comment parsed from the key line
    comment      TEXT,
    added_at     TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at TEXT,
    -- NULL while active; set to mark the key as no longer valid
    revoked_at   TEXT,
    -- Per-key target user — selects which host user's authorized_keys
    -- file the SSH admin page writes to when filesystem sync is enabled.
    -- NULL on existing rows; the sync skips unrouted keys and the admin
    -- UI surfaces them as "not synced" so an admin can fix them.
    target_user  TEXT
);

-- Short-lived tokens that grant access without a full login (e.g. CI/CD, scripts).
CREATE TABLE IF NOT EXISTS ssh_token
(
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    -- SHA-256 hash of the raw token; the plaintext is only shown once at creation
    token_hash TEXT    NOT NULL UNIQUE,
    label      TEXT    NOT NULL,
    -- Comma-separated scope list mirroring the session scopes (empty = full)
    scopes     TEXT    NOT NULL DEFAULT '',
    -- NULL means the token never expires
    expires_at TEXT,
    created_at TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Last time the token was successfully used
    used_at    TEXT,
    revoked_at TEXT
);

-- Per-key action log: who used which key, from where, and when.
CREATE TABLE IF NOT EXISTS ssh_session_audit
(
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER REFERENCES account (id) ON DELETE SET NULL,
    key_id     INTEGER REFERENCES ssh_key (id) ON DELETE SET NULL,
    -- Action identifier: "ssh.key.add", "ssh.key.revoke", "ssh.token.issue", etc.
    action     TEXT    NOT NULL,
    ip         TEXT,
    user_agent TEXT,
    created_at TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS ssh_key_fingerprint_idx ON ssh_key (account_id, fingerprint);
CREATE INDEX IF NOT EXISTS ssh_key_account_idx ON ssh_key (account_id);
CREATE INDEX IF NOT EXISTS ssh_token_account_idx ON ssh_token (account_id);
CREATE INDEX IF NOT EXISTS ssh_session_audit_account_idx ON ssh_session_audit (account_id);
CREATE INDEX IF NOT EXISTS ssh_session_audit_key_idx ON ssh_session_audit (key_id);
CREATE INDEX IF NOT EXISTS ssh_session_audit_created_idx ON ssh_session_audit (created_at);

PRAGMA user_version = 6;
