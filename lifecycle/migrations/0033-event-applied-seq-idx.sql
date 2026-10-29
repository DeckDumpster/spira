-- The continuity scan reads `applied = ? AND seq > ? ORDER BY seq`; seq is the key, so (applied,
-- seq) lets it seek and read in order instead of scanning the table.
CREATE INDEX event_applied_seq_idx ON event (applied, seq);
