-- Index for the per-key history read (`WHERE lc_key = ?`): event_lc_key_idx leads with machine,
-- so a read by lc_key alone cannot use it and scans the table. CREATE INDEX needs an admin.

CREATE INDEX event_history_idx ON event (lc_key, machine);
