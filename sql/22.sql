-- Revises: V21
-- Creation Date: 2026-07-14
-- Reason: Pastebin v4 — anonymous pastes (account_id nullable), titles, visibility,
--   password encryption, burn-after-read, edit tokens, forks, and revision history.
--   Rebuilds `paste` because SQLite cannot relax NOT NULL in place. Owned pastes keep
--   ON DELETE CASCADE (account deletion still takes them); anonymous rows (NULL owner)
--   are unaffected by it.
--
-- Rebuild safety (keep exactly this statement order): the pool turns `foreign_keys = ON`
-- before the migration hook runs, so this executes with FK enforcement live. It is still
-- safe — `DROP TABLE paste` implicitly deletes the old rows, and nothing referencing them
-- has any rows yet (`paste_new.fork_of` is all-NULL after the copy; `paste_revision` is
-- created only *after* the rename). SQLite resolves FK targets by table name at runtime,
-- so once `paste_new` is renamed, `fork_of TEXT REFERENCES paste (id)` points at the new
-- table — a correct self-reference.
--
-- `content` becomes BLOB: a password-protected paste stores ChaCha20-Poly1305 ciphertext,
-- which is not valid UTF-8. Plaintext pastes still hold UTF-8 bytes.

CREATE TABLE paste_new
(
    id              TEXT PRIMARY KEY,
    account_id      INTEGER REFERENCES account (id) ON DELETE CASCADE, -- NULL = anonymous
    title           TEXT,
    content         BLOB    NOT NULL,                                  -- plaintext UTF-8, or ciphertext when enc_salt IS NOT NULL
    language        TEXT,
    visibility      TEXT    NOT NULL DEFAULT 'unlisted',               -- 'public' | 'unlisted' | 'private'
    burn_after_read INTEGER NOT NULL DEFAULT 0,
    enc_salt        BLOB,                                              -- Argon2id salt; NULL = not password-protected
    enc_nonce       BLOB,                                              -- ChaCha20-Poly1305 nonce
    edit_token_hash TEXT,                                              -- SHA-256 of the anonymous edit/delete token
    size_bytes      INTEGER NOT NULL DEFAULT 0,
    fork_of         TEXT REFERENCES paste (id) ON DELETE SET NULL,
    creator_ip      TEXT,                                              -- takedown plumbing for anonymous pastes; never serialised
    views           INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT,
    expires_at      TEXT
);

-- Existing rows keep exactly today's behaviour: owned, unlisted, no password, no burn.
INSERT INTO paste_new (id, account_id, content, language, views, created_at, expires_at, size_bytes)
SELECT id, account_id, CAST(content AS BLOB), language, views, created_at, expires_at, length(content)
FROM paste;

DROP TABLE paste;
ALTER TABLE paste_new RENAME TO paste;

CREATE INDEX IF NOT EXISTS paste_account_idx ON paste (account_id);
CREATE INDEX IF NOT EXISTS paste_expires_idx ON paste (expires_at);
CREATE INDEX IF NOT EXISTS paste_visibility_idx ON paste (visibility, created_at DESC);

-- Edit history. Capped to the last N revisions per paste by the hourly reaper.
CREATE TABLE IF NOT EXISTS paste_revision
(
    id         INTEGER PRIMARY KEY,
    paste_id   TEXT    NOT NULL REFERENCES paste (id) ON DELETE CASCADE,
    content    BLOB    NOT NULL,
    title      TEXT,
    language   TEXT,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS paste_revision_idx ON paste_revision (paste_id, created_at DESC);
