-- A task's state past 64 KB failed every journal append; the column is the consumer to widen.
ALTER TABLE runtime_event MODIFY COLUMN kind_body LONGTEXT NOT NULL;
ALTER TABLE runtime_schedule MODIFY COLUMN kind_body LONGTEXT NOT NULL;
