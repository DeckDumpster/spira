-- The where-stuck read model: applied and refused edges by window, and each live bead's time in
-- its state against that state's measured p95 dwell. ops_dwell_p95 is the one place a threshold
-- comes from; nothing hand-sets one.

-- Leads with the two equalities the views filter on, then `at`, so the window is a range on the
-- index rather than a filter over every bead event; the tail columns make it covering.
CREATE INDEX event_at_idx ON event (machine, applied, at, from_state, to_state, event, refusal);

CREATE VIEW ops_edges AS
SELECT 'applied' AS kind, from_state, to_state, NULL AS event, NULL AS refusal,
       SUM(at >= UNIX_TIMESTAMP() - 3600) AS n_1h, COUNT(*) AS n_24h
  FROM event
 WHERE machine = 'bead' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 86400 AND from_state <> to_state
 GROUP BY from_state, to_state
UNION ALL
SELECT 'refused', from_state, NULL, event, refusal,
       SUM(at >= UNIX_TIMESTAMP() - 3600), COUNT(*)
  FROM event
 WHERE machine = 'bead' AND applied = 0 AND at >= UNIX_TIMESTAMP() - 86400
 GROUP BY from_state, event, refusal;

CREATE VIEW ops_dwell_p95 AS
SELECT state, MIN(dwell) AS p95_s, MAX(n) AS samples
  FROM (SELECT state, dwell,
               ROW_NUMBER() OVER (PARTITION BY state ORDER BY dwell) AS rn,
               COUNT(*) OVER (PARTITION BY state) AS n
          FROM (SELECT from_state AS state,
                       at - LAG(at) OVER (PARTITION BY lc_key ORDER BY seq) AS dwell
                  FROM event
                 WHERE machine = 'bead' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 1209600
                   AND from_state <> to_state) d
         WHERE dwell IS NOT NULL) r
 WHERE rn >= CEIL(0.95 * n)
 GROUP BY state;

CREATE VIEW ops_dwell AS
SELECT x.bead_id, x.state, x.holds, x.priority, x.title, x.entered_at, p.p95_s, p.samples
  FROM (SELECT /*+ JOIN_ORDER(r,t,e) LOOKUP_JOIN(r,t) LOOKUP_JOIN(t,e) */
               t.bead_id, t.state, t.holds, t.priority, t.title,
               COALESCE(MAX(e.at), t.updated_at) AS entered_at
          FROM (SELECT bead_id FROM bead
                 WHERE state IN ('OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK')) r
          JOIN bead t ON t.bead_id = r.bead_id
          LEFT JOIN event e ON e.machine = 'bead' AND e.applied = 1 AND e.lc_key = t.bead_id
                           AND e.to_state = t.state AND e.from_state <> e.to_state
         GROUP BY t.bead_id, t.state, t.holds, t.priority, t.title, t.updated_at) x
  LEFT JOIN ops_dwell_p95 p ON p.state = x.state;
