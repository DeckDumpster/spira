-- A pre-bound event id was cut to the column width by INSERT IGNORE. No id derivation yields
-- exactly 255 characters (lengths are 14+18n, 26+18n, 34+18n and +8), so CHAR_LENGTH = 255
-- marks a truncated row. Marking it applied for the one registered handler, heartbeat, makes
-- dispatch skip it without deleting; its schedule row fires again under a bounded id.
INSERT INTO runtime_applied (handler, event_id)
SELECT 'heartbeat', runtime_event.event_id FROM runtime_event WHERE CHAR_LENGTH(runtime_event.event_id) = 255
ON DUPLICATE KEY UPDATE handler = handler;
