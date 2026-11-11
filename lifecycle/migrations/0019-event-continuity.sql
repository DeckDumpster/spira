-- One-time correction: an applied bead Deliver or delivery Cut event records the state its
-- predecessor event left the row in, not the state the cascade once hard-coded. Idempotent.
-- A `-- probe:` line is the applied-probe for the statement below it: the statement's own
-- WHERE is a correlated scan of the whole event table.

-- probe: SELECT 1 FROM (SELECT from_state, machine, event, LAG(to_state) OVER (PARTITION BY machine, lc_key ORDER BY seq) AS prev FROM event WHERE applied = 1) x WHERE ((machine = 'bead' AND event = 'Deliver') OR (machine = 'delivery' AND event = 'Cut')) AND prev IS NOT NULL AND from_state <> prev LIMIT 1
UPDATE event
   SET from_state = (SELECT p.to_state FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq ORDER BY p.seq DESC LIMIT 1),
       expect = (SELECT p.to_state FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq ORDER BY p.seq DESC LIMIT 1)
 WHERE applied = 1 AND ((machine = 'bead' AND event = 'Deliver') OR (machine = 'delivery' AND event = 'Cut'))
   AND EXISTS (SELECT 1 FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq)
   AND from_state <> (SELECT p.to_state FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq ORDER BY p.seq DESC LIMIT 1);

-- probe: SELECT 1 FROM (SELECT from_state, machine, event, ROW_NUMBER() OVER (PARTITION BY machine, lc_key ORDER BY seq) AS rn FROM event WHERE applied = 1) x WHERE machine = 'delivery' AND event = 'Cut' AND rn = 1 AND from_state <> 'NONE' LIMIT 1
UPDATE event
   SET from_state = 'NONE', expect = 'NONE'
 WHERE applied = 1 AND machine = 'delivery' AND event = 'Cut' AND from_state <> 'NONE'
   AND NOT EXISTS (SELECT 1 FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq);
