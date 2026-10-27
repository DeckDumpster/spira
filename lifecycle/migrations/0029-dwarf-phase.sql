-- Adds bead.dwarf_phase for an existing spira_lifecycle database: the holder's phase, set only
-- while WORKING and read by nothing after the holder exits, so the cutover from the former
-- column needs no copy. A fresh database gets the column from schema.sql.

ALTER TABLE bead ADD COLUMN dwarf_phase VARCHAR(16) NULL;
