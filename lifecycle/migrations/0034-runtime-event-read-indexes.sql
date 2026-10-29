-- Opening the runtime reads one stream's history and the unresolved effect requests; without
-- these two indexes each is a scan of the whole journal.
CREATE INDEX runtime_event_stream_idx ON runtime_event (stream, seq);
CREATE INDEX runtime_event_cause_idx ON runtime_event (cause, kind_tag);
