-- Adds delivery.ci for an existing spira_lifecycle database: the forge CI sub-state of a
-- PUBLISHING row. A fresh database gets the column from schema.sql.

ALTER TABLE delivery ADD COLUMN ci VARCHAR(8) NULL;
