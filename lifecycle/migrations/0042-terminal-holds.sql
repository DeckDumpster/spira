-- One-time correction: terminal rows must carry no hold. Idempotent.

UPDATE bead SET holds = JSON_ARRAY()
 WHERE state IN ('LANDED', 'SUPERSEDED', 'DROPPED', 'DONE')
   AND JSON_LENGTH(holds) > 0;
