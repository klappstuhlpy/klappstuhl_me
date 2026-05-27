CREATE TABLE IF NOT EXISTS file_scan (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    filename      TEXT    NOT NULL,
    file_size     INTEGER NOT NULL,
    sha256        TEXT    NOT NULL,
    -- ClamAV: NULL = not checked, 1 = clean, 0 = infected
    clamav_clean  INTEGER,
    clamav_virus  TEXT,
    -- VirusTotal: "clean" | "detected" | "unknown" | "error" | NULL = not checked
    vt_status     TEXT,
    vt_positives  INTEGER,
    vt_total      INTEGER,
    vt_url        TEXT,
    scanned_at    TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

PRAGMA user_version = 8;
