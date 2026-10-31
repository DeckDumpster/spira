-- The mending key <batch>#<pass>#<suite> outgrew its columns: batch ids and suite names keep
-- getting longer. batch_id matches batch.batch_id; the event key holds the widest triple.
-- A fresh database gets the widths from schema.sql.

ALTER TABLE mending MODIFY COLUMN batch_id VARCHAR(64) NOT NULL;
ALTER TABLE mending MODIFY COLUMN suite VARCHAR(160) NOT NULL;
ALTER TABLE mend MODIFY COLUMN batch_id VARCHAR(64) NOT NULL;
ALTER TABLE mend MODIFY COLUMN suite VARCHAR(160) NOT NULL;
ALTER TABLE event MODIFY COLUMN lc_key VARCHAR(256) NOT NULL;
