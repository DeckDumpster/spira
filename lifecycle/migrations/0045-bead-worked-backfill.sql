-- Backfills bead.worked_s and bead.sessions from the event log in one grouped pass; a bead in
-- WORKING has its open session left out of worked_s. Idempotent.

UPDATE bead JOIN (SELECT lc_key,
                         SUM(to_state = 'WORKING') AS n,
                         SUM(IF(from_state = 'WORKING', at, 0)) - SUM(IF(to_state = 'WORKING', at, 0)) AS d
                    FROM event
                   WHERE machine = 'bead' AND applied = 1 AND from_state <> to_state
                     AND 'WORKING' IN (from_state, to_state)
                   GROUP BY lc_key) w
    ON w.lc_key = bead.bead_id
   SET bead.sessions = w.n,
       bead.worked_s = GREATEST(0, w.d + IF(bead.state = 'WORKING', COALESCE(bead.since, 0), 0))
 WHERE NOT (bead.sessions <=> w.n)
    OR NOT (bead.worked_s <=> GREATEST(0, w.d + IF(bead.state = 'WORKING', COALESCE(bead.since, 0), 0)));
