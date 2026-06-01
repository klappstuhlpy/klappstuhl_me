-- Preserve the uploader's original filename alongside the random public ID.
-- The ID still owns the URL (opaque, non-enumerable); original_name is used
-- only for human-friendly downloads (Content-Disposition) and ZIP entry names.
-- NULL for rows uploaded before this migration.
ALTER TABLE images ADD COLUMN original_name TEXT;
