-- Adds bead.repo to the bead rows of ops_live, ops_recent and ops_dwell, so the ops pane can
-- tag each row with its repository. `repo` is the last alias of each view: migration probing
-- reads it to tell this replacement has been applied. ops_round already carries the batch's repo.
CREATE OR REPLACE VIEW ops_live AS
SELECT /*+ JOIN_ORDER(r,t,x) LOOKUP_JOIN(r,t) */
       t.bead_id, t.state, t.holds, t.holder, t.persona, (t.state = 'REWORK') AS rework, t.lease_until, t.since, t.updated_at, t.priority, t.title,
       (t.state = 'READY' AND JSON_LENGTH(t.holds) = 0 AND x.blocker IS NULL) AS claimable,
       x.blocker AS blocker,
       t.repo AS repo
  FROM (SELECT bead_id FROM bead
         WHERE state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) r
  JOIN bead t ON t.bead_id = r.bead_id
  LEFT JOIN (SELECT /*+ JOIN_ORDER(b,d) LOOKUP_JOIN(b,d) */ d.bead_id, MIN(d.depends_on) AS blocker
               FROM bead b JOIN bead_dep d ON d.depends_on = b.bead_id
              WHERE b.state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')
                AND d.dep_type = 'blocks'
              GROUP BY d.bead_id) x ON x.bead_id = t.bead_id;

CREATE OR REPLACE VIEW ops_recent AS
SELECT /*+ JOIN_ORDER(r,t) LOOKUP_JOIN(r,t) */
       t.bead_id, t.state, t.since, t.updated_at, t.priority, t.title, t.repo AS repo
  FROM (SELECT bead_id FROM bead WHERE state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400) r
  JOIN bead t ON t.bead_id = r.bead_id;

CREATE OR REPLACE VIEW ops_dwell AS
SELECT x.bead_id, x.state, x.holds, x.priority, x.title, x.entered_at, p.p95_s, p.samples, x.repo AS repo
  FROM (SELECT /*+ JOIN_ORDER(r,t,e) LOOKUP_JOIN(r,t) LOOKUP_JOIN(t,e) */
               t.bead_id, t.state, t.holds, t.priority, t.title, t.repo,
               COALESCE(MAX(e.at), t.updated_at) AS entered_at
          FROM (SELECT bead_id FROM bead
                 WHERE state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) r
          JOIN bead t ON t.bead_id = r.bead_id
          LEFT JOIN event e ON e.machine = 'bead' AND e.applied = 1 AND e.lc_key = t.bead_id
                           AND e.to_state = t.state AND e.from_state <> e.to_state
         GROUP BY t.bead_id, t.state, t.holds, t.priority, t.title, t.repo, t.updated_at) x
  LEFT JOIN ops_dwell_p95 p ON p.state = x.state;
