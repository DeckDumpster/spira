-- Dependency edges mirrored from bd (written by bead.sh dep add/remove, backfilled once by
-- `spira-lc backfill-deps`), so the lifecycle can say whether a READY bead is claimable and
-- what blocks it. bead_id depends on depends_on; only a `blocks` edge to a bead still in a
-- live state blocks. Other edge types are kept for the record and never block.
CREATE TABLE IF NOT EXISTS bead_dep (
    bead_id    VARCHAR(64) NOT NULL,
    depends_on VARCHAR(64) NOT NULL,
    dep_type   VARCHAR(32) NOT NULL,
    PRIMARY KEY (bead_id, depends_on)
);

CREATE INDEX bead_dep_target_idx ON bead_dep (depends_on);

-- The columns appended to ops_live: claimable is a READY, unheld bead with no live blocker;
-- blocker is the lowest-id live bead it waits on. Keep the last select alias `blocker`:
-- migration probing reads it to tell this replacement has been applied.
CREATE OR REPLACE VIEW ops_live AS
SELECT /*+ JOIN_ORDER(r,t) LOOKUP_JOIN(r,t) */
       t.bead_id, t.state, t.holds, t.holder, t.lease_until, t.since, t.updated_at, t.priority, t.title,
       (t.state = 'READY' AND JSON_LENGTH(t.holds) = 0 AND NOT EXISTS
           (SELECT 1 FROM bead_dep d JOIN bead b ON b.bead_id = d.depends_on
             WHERE d.bead_id = t.bead_id AND d.dep_type = 'blocks'
               AND b.state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK'))) AS claimable,
       (SELECT MIN(d.depends_on) FROM bead_dep d JOIN bead b ON b.bead_id = d.depends_on
         WHERE d.bead_id = t.bead_id AND d.dep_type = 'blocks'
           AND b.state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) AS blocker
  FROM (SELECT bead_id FROM bead
         WHERE state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) r
  JOIN bead t ON t.bead_id = r.bead_id;
