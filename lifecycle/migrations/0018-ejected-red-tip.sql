-- Adds bead.ejected_red_tip for an existing spira_lifecycle database: the tip a round last ejected
-- as red, which submit refuses. A fresh database gets the column from schema.sql.

ALTER TABLE bead ADD COLUMN ejected_red_tip VARCHAR(64) NULL;
