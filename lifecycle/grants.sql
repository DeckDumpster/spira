-- grants.sql — the enforcement in design §3.3/§3.6: only spira_lc writes spira_lifecycle,
-- and even spira_lc cannot rewrite the event log. Applied once, as a superuser, after
-- schema.sql:
--
--   dolt --host H --port P -u root -p ROOTPASS sql < grants.sql
--
-- @SPIRA_LC_PASSWORD@ is substituted by whatever creates the user (install.sh's
-- --system-user step in production; the test fixture in the container tier). This file is
-- never committed with a real password in it — the placeholder is the point. The ALTER USER
-- after each CREATE USER IF NOT EXISTS sets the password on every apply, so a rotated
-- credential converges instead of diverging.
CREATE USER IF NOT EXISTS 'spira_lc'@'%' IDENTIFIED BY '@SPIRA_LC_PASSWORD@';
ALTER USER 'spira_lc'@'%' IDENTIFIED BY '@SPIRA_LC_PASSWORD@';

GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.bead TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.delivery TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.batch TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.batch_member TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.ask TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.mending TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE, DELETE ON spira_lifecycle.bead_dep TO 'spira_lc'@'%';

-- The load-bearing line: INSERT and SELECT only. No UPDATE, no DELETE — verified to hold
-- in Dolt as a table-level grant (design §3.3's throwaway-server findings, 2026-09-26).
-- `event` is backtick-quoted here only: GRANT's grammar reserves the bare word (it is not
-- reserved for CREATE TABLE, SELECT, INSERT or UPDATE, which is why nothing else in this
-- schema quotes it) — found by running this file against a real Dolt 2.2.3 server.
GRANT SELECT, INSERT ON spira_lifecycle.`event` TO 'spira_lc'@'%';

-- A second, read-only user: SELECT on every table, nothing else, for callers that only ever
-- read (spira-lc history, tsd-lifecycle-export) and so must never hold spira_lc's own
-- INSERT/UPDATE authority. @SPIRA_LC_RO_PASSWORD@ is substituted the same way as
-- @SPIRA_LC_PASSWORD@ above, and independently of it — the two credentials are never the
-- same secret.
CREATE USER IF NOT EXISTS 'spira_lc_ro'@'%' IDENTIFIED BY '@SPIRA_LC_RO_PASSWORD@';
ALTER USER 'spira_lc_ro'@'%' IDENTIFIED BY '@SPIRA_LC_RO_PASSWORD@';

GRANT SELECT ON spira_lifecycle.bead TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.delivery TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.batch TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.batch_member TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.ask TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.mending TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.bead_dep TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.`event` TO 'spira_lc_ro'@'%';

-- The ops read model (migrations/0007-ops-read-model.sql): the views the panes read. spira_lc
-- serves them (`spira-lc ops-view`) and must see them in information_schema for the
-- migration probe to find them applied.
GRANT SELECT ON spira_lifecycle.ops_live TO 'spira_lc'@'%';
GRANT SELECT ON spira_lifecycle.ops_round TO 'spira_lc'@'%';
GRANT SELECT ON spira_lifecycle.ops_recent TO 'spira_lc'@'%';
GRANT SELECT ON spira_lifecycle.ops_live TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.ops_round TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.ops_recent TO 'spira_lc_ro'@'%';

-- The where-stuck read model (migrations/0009-where-stuck.sql).
GRANT SELECT ON spira_lifecycle.ops_edges TO 'spira_lc'@'%';
GRANT SELECT ON spira_lifecycle.ops_edges TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.ops_dwell_p95 TO 'spira_lc'@'%';
GRANT SELECT ON spira_lifecycle.ops_dwell_p95 TO 'spira_lc_ro'@'%';
GRANT SELECT ON spira_lifecycle.ops_dwell TO 'spira_lc'@'%';
GRANT SELECT ON spira_lifecycle.ops_dwell TO 'spira_lc_ro'@'%';
