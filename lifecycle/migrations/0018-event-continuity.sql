-- One-time correction: an applied bead Deliver or delivery Cut event records the state its
-- predecessor event left the row in, not the state the cascade once hard-coded. Idempotent.

UPDATE event
   SET from_state = (SELECT p.to_state FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq ORDER BY p.seq DESC LIMIT 1),
       expect = (SELECT p.to_state FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq ORDER BY p.seq DESC LIMIT 1)
 WHERE applied = 1 AND ((machine = 'bead' AND event = 'Deliver') OR (machine = 'delivery' AND event = 'Cut'))
   AND EXISTS (SELECT 1 FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq)
   AND from_state <> (SELECT p.to_state FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq ORDER BY p.seq DESC LIMIT 1);

UPDATE event
   SET from_state = 'NONE', expect = 'NONE'
 WHERE applied = 1 AND machine = 'delivery' AND event = 'Cut' AND from_state <> 'NONE'
   AND NOT EXISTS (SELECT 1 FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq);
