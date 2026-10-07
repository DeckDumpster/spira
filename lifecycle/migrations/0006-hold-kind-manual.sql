-- One-time rename of the `operator` hold kind to `manual`; a timed snooze held under it is a
-- `wait` hold. Idempotent: each statement stops matching once its rows are rewritten.

UPDATE bead
   SET holds = REPLACE(CAST(holds AS CHAR), '"operator"', '"wait"')
 WHERE CAST(holds AS CHAR) LIKE '%"operator"%'
   AND reason LIKE 'snooze-until:%';

UPDATE bead
   SET holds = REPLACE(CAST(holds AS CHAR), '"operator"', '"manual"')
 WHERE CAST(holds AS CHAR) LIKE '%"operator"%';
