-- bead.since is the time of the row's last change of state: backfilled from the event log in one
-- grouped pass (a correlated subquery per bead does not finish on a real log), and stamped by
-- every applied state change from here on. Idempotent.

UPDATE bead JOIN (SELECT lc_key, to_state, MAX(at) AS last_at FROM event
                   WHERE machine = 'bead' AND applied = 1 AND from_state <> to_state
                   GROUP BY lc_key, to_state) latest
    ON latest.lc_key = bead.bead_id AND latest.to_state = bead.state
   SET bead.since = latest.last_at
 WHERE NOT (bead.since <=> latest.last_at);

UPDATE bead SET since = updated_at WHERE since IS NULL;
