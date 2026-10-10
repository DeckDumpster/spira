-- Covering index for the grouped latest-event-per-(key, state) read behind `list`/`list-delivery`.

CREATE INDEX event_since_idx ON event (machine, applied, lc_key, to_state, at);
