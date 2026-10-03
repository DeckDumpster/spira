-- One-time correction: terminal rows must carry no holder or lease. Idempotent.

UPDATE bead
   SET holder = NULL, lease_until = NULL
 WHERE state IN ('LANDED', 'SUPERSEDED', 'DROPPED', 'DONE')
   AND (holder IS NOT NULL OR lease_until IS NOT NULL);
