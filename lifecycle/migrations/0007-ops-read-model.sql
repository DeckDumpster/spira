-- The ops read model: title and priority mirrored into the bead row, the indexes the views
-- need, and the three views the panes read instead of scanning bd.

ALTER TABLE bead ADD COLUMN title VARCHAR(512) NULL;
ALTER TABLE bead ADD COLUMN priority TINYINT NULL;

CREATE INDEX bead_state_since_idx ON bead (state, since);
CREATE INDEX batch_state_idx ON batch (state);

-- Each view takes its keys from the covering (state, since) index, then reads the rows by
-- primary key: with the hint the plan is the same whether or not the optimizer has table
-- statistics, which on a mostly-terminal table would otherwise choose a scan.
CREATE VIEW ops_live AS
SELECT /*+ JOIN_ORDER(r,t) LOOKUP_JOIN(r,t) */
       t.bead_id, t.state, t.holds, t.holder, t.persona, (t.state = 'REWORK') AS rework, t.lease_until, t.since, t.updated_at, t.priority, t.title
  FROM (SELECT bead_id FROM bead
         WHERE state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) r
  JOIN bead t ON t.bead_id = r.bead_id;

CREATE VIEW ops_round AS
SELECT /*+ JOIN_ORDER(b,m,t) LOOKUP_JOIN(b,m) LOOKUP_JOIN(m,t) */
       b.batch_id, b.repo, b.state AS batch_state, b.head, b.base, b.opened_at,
       m.bead_id, m.tip, m.outcome, t.title, t.priority
  FROM batch b
  JOIN batch_member m ON m.batch_id = b.batch_id
  LEFT JOIN bead t ON t.bead_id = m.bead_id
 WHERE b.state IN ('OPEN', 'CI_RUNNING', 'GREEN', 'ATTRIBUTING', 'REBUILDING');

CREATE VIEW ops_recent AS
SELECT /*+ JOIN_ORDER(r,t) LOOKUP_JOIN(r,t) */
       t.bead_id, t.state, t.since, t.updated_at, t.priority, t.title
  FROM (SELECT bead_id FROM bead WHERE state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400) r
  JOIN bead t ON t.bead_id = r.bead_id;
