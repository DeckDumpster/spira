-- bead.since is the time of the row's last change of state: backfilled from the event log, and
-- stamped by every applied state change from here on. Idempotent.

UPDATE bead
   SET since = COALESCE((SELECT MAX(e.at) FROM event e
                          WHERE e.machine = 'bead' AND e.applied = 1 AND e.lc_key = bead.bead_id
                            AND e.to_state = bead.state AND NOT (e.from_state <=> e.to_state)), bead.updated_at)
 WHERE since IS NULL
    OR NOT (since <=> COALESCE((SELECT MAX(e.at) FROM event e
                          WHERE e.machine = 'bead' AND e.applied = 1 AND e.lc_key = bead.bead_id
                            AND e.to_state = bead.state AND NOT (e.from_state <=> e.to_state)), bead.updated_at));
