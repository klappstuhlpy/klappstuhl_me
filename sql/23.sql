-- Revises: 22
-- Reason: Drop the admin observability tables — metrics, Docker and health.
--
-- The admin control plane was extracted into the standalone Vantage app
-- (klappstuhlpy/vantage) in commit 13129c0, which removed `src/admin/` wholesale.
-- Vantage records this data in its *own* database, so these tables have had no
-- reader on this side since — they only sit in the schema collecting writes that
-- never come.
--
-- `IF EXISTS` throughout: a database created after the extraction never ran the
-- migrations that made these (they are still in 3.sql/9.sql, so it did — but the
-- guard keeps this file correct regardless of which states exist in the wild,
-- which is the same reason 20.sql had to rebuild a table).
--
-- Order matters with `foreign_keys = ON`: health_check_sample and health_incident
-- both REFERENCE health_target (9.sql), so the children go first. Dropping a
-- table drops its indexes with it — they need no separate statement.

-- Host and container metrics (3.sql).
DROP TABLE IF EXISTS metric_sample;
DROP TABLE IF EXISTS docker_stat;
DROP TABLE IF EXISTS docker_snapshot;

-- Uptime monitoring (9.sql). Children first — see above.
DROP TABLE IF EXISTS health_check_sample;
DROP TABLE IF EXISTS health_incident;
DROP TABLE IF EXISTS health_target;
