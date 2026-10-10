-- Adds bead.sifted_tip for an existing spira_lifecycle database: the tip the pre-round screen last
-- passed (the Sifted event). A fresh database gets the column from schema.sql.

ALTER TABLE bead ADD COLUMN sifted_tip VARCHAR(64) NULL;
