-- Features: images, short links, pastes, and paste revisions.

CREATE TABLE IF NOT EXISTS images
(
    id            TEXT    NOT NULL PRIMARY KEY,
    image_data    BLOB    NOT NULL,
    size          INTEGER GENERATED ALWAYS AS (length(image_data)) STORED,
    mimetype      TEXT    NOT NULL,
    uploaded_at   TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    uploader_id   INTEGER REFERENCES account (id) ON DELETE SET NULL,
    original_name TEXT,
    views         INTEGER NOT NULL DEFAULT 0,
    expires_at    TEXT,
    guild_id      TEXT
);

CREATE INDEX IF NOT EXISTS image_idx ON images (id);
CREATE INDEX IF NOT EXISTS images_guild_idx ON images (guild_id);


CREATE TABLE IF NOT EXISTS short_link
(
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    code        TEXT    NOT NULL UNIQUE,
    target_url  TEXT    NOT NULL,
    account_id  INTEGER NOT NULL REFERENCES account (id) ON DELETE CASCADE,
    clicks      INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX IF NOT EXISTS short_link_account_idx ON short_link (account_id);


CREATE TABLE IF NOT EXISTS paste
(
    id              TEXT PRIMARY KEY,
    account_id      INTEGER REFERENCES account (id) ON DELETE CASCADE,
    title           TEXT,
    content         BLOB    NOT NULL,
    language        TEXT,
    visibility      TEXT    NOT NULL DEFAULT 'unlisted',
    burn_after_read INTEGER NOT NULL DEFAULT 0,
    enc_salt        BLOB,
    enc_nonce       BLOB,
    edit_token_hash TEXT,
    size_bytes      INTEGER NOT NULL DEFAULT 0,
    fork_of         TEXT REFERENCES paste (id) ON DELETE SET NULL,
    creator_ip      TEXT,
    views           INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT,
    expires_at      TEXT
);

CREATE INDEX IF NOT EXISTS paste_account_idx ON paste (account_id);
CREATE INDEX IF NOT EXISTS paste_expires_idx ON paste (expires_at);
CREATE INDEX IF NOT EXISTS paste_visibility_idx ON paste (visibility, created_at DESC);


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
