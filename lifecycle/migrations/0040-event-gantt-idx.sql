-- ops-gantt reads seq, lc_key, event, from_state, to_state and at of a day of applied transitions.
-- event_at_idx lacks lc_key, so every row cost a lookup into the table; this one carries every
-- column the read selects, so the day's window is a range read of the index alone.
CREATE INDEX event_gantt_idx ON event (machine, applied, at, from_state, to_state, event, lc_key);
