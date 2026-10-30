-- Adds the dispatcher's last verdict on a READY or REWORK bead to its row, so the ops views
-- read why a bead is or is not claimable from the read model they already load. Written by
-- `spira-lc claim-verdicts` once per dispatch pass; read by nothing that transitions the bead.
-- A fresh database gets the columns from schema.sql.

ALTER TABLE bead ADD COLUMN claim_verdict VARCHAR(16) NULL;
ALTER TABLE bead ADD COLUMN claim_blocker VARCHAR(64) NULL;
ALTER TABLE bead ADD COLUMN claim_runes VARCHAR(255) NULL;
ALTER TABLE bead ADD COLUMN claim_at VARCHAR(20) NULL;
