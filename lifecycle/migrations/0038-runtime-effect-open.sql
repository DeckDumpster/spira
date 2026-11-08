-- The runtime's boot read of unresolved effect requests walked the whole journal, so it grew
-- with history. `runtime_effect` holds one row per request: resolved = 0 until an outcome
-- names it as cause, and `runtime_effect_open_idx` makes the boot read a seek.
-- A fresh database gets the table from schema.sql.

CREATE TABLE IF NOT EXISTS runtime_effect (
    event_id VARCHAR(255) NOT NULL PRIMARY KEY,
    seq      BIGINT NOT NULL,
    resolved TINYINT(1) NOT NULL DEFAULT 0
);
CREATE INDEX runtime_effect_open_idx ON runtime_effect (resolved, seq);

INSERT INTO runtime_effect (event_id, seq, resolved)
SELECT runtime_event.event_id, runtime_event.seq, 0 FROM runtime_event WHERE runtime_event.kind_tag = 'effect.requested' AND NOT EXISTS (SELECT 1 FROM runtime_event o WHERE o.cause = runtime_event.event_id AND o.kind_tag IN ('effect.succeeded', 'effect.failed', 'effect.timed_out')) AND NOT EXISTS (SELECT 1 FROM runtime_effect e WHERE e.event_id = runtime_event.event_id);
