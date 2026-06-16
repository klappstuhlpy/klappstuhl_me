CREATE TABLE IF NOT EXISTS docker_snapshot (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    container_id   TEXT    NOT NULL,
    container_name TEXT    NOT NULL,
    original_image TEXT    NOT NULL,
    snapshot_tag   TEXT    NOT NULL UNIQUE,
    description    TEXT,
    created_at     TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);
