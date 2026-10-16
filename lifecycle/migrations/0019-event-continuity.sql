-- One-time correction: an applied bead Deliver or delivery Cut event records the state its
-- predecessor event left the row in, not the state the cascade once hard-coded. Idempotent.

UPDATE event
  JOIN (SELECT seq AS pseq, LAG(to_state) OVER (PARTITION BY machine, lc_key ORDER BY seq) AS prev FROM event WHERE applied = 1) s ON s.pseq = event.seq
   SET event.from_state = s.prev, event.expect = s.prev
 WHERE event.applied = 1 AND ((event.machine = 'bead' AND event.event = 'Deliver') OR (event.machine = 'delivery' AND event.event = 'Cut'))
   AND s.prev IS NOT NULL AND event.from_state <> s.prev;

UPDATE event
   SET from_state = 'NONE', expect = 'NONE'
 WHERE applied = 1 AND machine = 'delivery' AND event = 'Cut' AND from_state <> 'NONE'
   AND NOT EXISTS (SELECT 1 FROM event p WHERE p.machine = event.machine AND p.lc_key = event.lc_key AND p.applied = 1 AND p.seq < event.seq);
