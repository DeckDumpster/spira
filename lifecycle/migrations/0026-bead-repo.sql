-- Adds bead.repo for an existing spira_lifecycle database: the repo:<name> a bead was filed
-- against, so a pool is selected by repo without reading the content store. Existing rows are
-- filled once by `spira-lc backfill-repos`. A fresh database gets the column from schema.sql.

ALTER TABLE bead ADD COLUMN repo VARCHAR(64) NULL;
