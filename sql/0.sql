-- Storage related information for directory entries
-- This has to be a little bit complicated to power the fact
-- that multiple searches are allowed

-- Do note that SQLite does *not* allow atomic ALTER TABLE
-- So this schema is pretty much final unless you leave it
-- to atomic operations like CREATE TABLE and CREATE INDEX

CREATE TABLE IF NOT EXISTS images
(
    id          TEXT    NOT NULL PRIMARY KEY,
    image_data  BLOB    NOT NULL,
    size        INTEGER GENERATED ALWAYS AS (length(image_data)) STORED,
    mimetype    TEXT    NOT NULL,
    uploaded_at TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    uploader_id INTEGER REFERENCES account (id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS image_idx ON images (id);


-- This is for the authentication aspect
-- Note that usernames are all lowercase
-- Email is *not* stored anywhere
CREATE TABLE IF NOT EXISTS account
(
    id          INTEGER PRIMARY KEY,
    name        TEXT UNIQUE NOT NULL,
    password    TEXT        NOT NULL,
    created_at  TEXT        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    flags       INTEGER     NOT NULL DEFAULT 0,
    invite_code TEXT        NOT NULL DEFAULT '_console',
    totp_secret TEXT,
    totp_enabled INTEGER    NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS account_name_idx ON account (name);

-- This is a long form key: value storage that can be used for any type of
-- generic data that doesn't belong in a normalized table.
-- Due to the dynamic typing nature of SQLite that we're abusing, the value
-- can technically have any type.
CREATE TABLE IF NOT EXISTS storage
(
    name  TEXT PRIMARY KEY,
    value TEXT
) WITHOUT ROWID;

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

PRAGMA user_version = 1;
