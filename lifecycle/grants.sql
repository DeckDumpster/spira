-- grants.sql — the enforcement in design §3.3/§3.6: only spira_lc writes spira_lifecycle,
-- and even spira_lc cannot rewrite the event log. Applied once, as a superuser, after
-- schema.sql:
--
--   dolt --host H --port P -u root -p ROOTPASS sql < grants.sql
--
-- @SPIRA_LC_PASSWORD@ is substituted by whatever creates the user (install.sh's
-- --system-user step in production; the test fixture in the container tier). This file is
-- never committed with a real password in it — the placeholder is the point.
CREATE USER IF NOT EXISTS 'spira_lc'@'%' IDENTIFIED BY '@SPIRA_LC_PASSWORD@';

GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.bead TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.delivery TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.batch TO 'spira_lc'@'%';
GRANT SELECT, INSERT, UPDATE ON spira_lifecycle.batch_member TO 'spira_lc'@'%';

-- The load-bearing line: INSERT and SELECT only. No UPDATE, no DELETE — verified to hold
-- in Dolt as a table-level grant (design §3.3's throwaway-server findings, 2026-09-26).
-- `event` is backtick-quoted here only: GRANT's grammar reserves the bare word (it is not
-- reserved for CREATE TABLE, SELECT, INSERT or UPDATE, which is why nothing else in this
-- schema quotes it) — found by running this file against a real Dolt 2.2.3 server.
GRANT SELECT, INSERT ON spira_lifecycle.`event` TO 'spira_lc'@'%';
