-- Per-image view counter, incremented each time the landing page is opened.
-- Approximate by design (no per-viewer dedup); legacy rows start at 0.
ALTER TABLE images ADD COLUMN views INTEGER NOT NULL DEFAULT 0;
