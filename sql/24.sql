-- Revises: 23
-- Reason: Drop the admin control-surface tables — firewall, proxy, secrets,
--         the file sanitizer, and SSH key management.
--
-- The second half of the Vantage extraction (see 23.sql). Every one of these
-- backed a panel that now lives in the standalone admin app against its own
-- database.
--
-- Note the site kept the *behaviour* these tables were nearest to where it still
-- needs it: login brute-force throttling is now in-process (`account::lockout`)
-- and no longer reaches into a firewall table, which is what lets it work whether
-- or not Vantage is running. The table itself has no reader either way.
--
-- Order matters with `foreign_keys = ON`: ssh_session_audit REFERENCES ssh_key
-- (6.sql), so the child goes first.

-- Firewall rule manager and its brute-force lockouts (10.sql).
DROP TABLE IF EXISTS firewall_rule;
DROP TABLE IF EXISTS firewall_lockout;

-- Reverse-proxy config generation (11.sql).
DROP TABLE IF EXISTS proxy_route;

-- Secrets scanner (4.sql).
DROP TABLE IF EXISTS secret_finding;

-- File sanitizer: ClamAV / VirusTotal scan results (5.sql).
DROP TABLE IF EXISTS file_scan;
DROP TABLE IF EXISTS scan_run;

-- SSH key store and its audit trail (6.sql). Child first — see above.
DROP TABLE IF EXISTS ssh_session_audit;
DROP TABLE IF EXISTS ssh_token;
DROP TABLE IF EXISTS ssh_key;
