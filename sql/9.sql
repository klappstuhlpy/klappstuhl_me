-- Revises: V8
-- Creation Date: 2026-05-27
-- Reason: Per-key target user — selects which host user's authorized_keys
--        file the SSH admin page writes to when filesystem sync is enabled.
--        NULL on existing rows; the sync skips unrouted keys and the admin
--        UI surfaces them as "not synced" so an admin can fix them.

ALTER TABLE ssh_key ADD COLUMN target_user TEXT;

CREATE INDEX IF NOT EXISTS ssh_key_target_user_idx ON ssh_key (target_user);

PRAGMA user_version = 9;
