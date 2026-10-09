-- ops_edges names the event behind each applied move: applied rows group by (from, to, event), so
-- a consumer that sums per (from, to) still gets the same totals. last_at is the view's last
-- select alias: migration probing reads it to tell this replacement has been applied.
CREATE OR REPLACE VIEW ops_edges AS
SELECT 'applied' AS kind, from_state, to_state, event, NULL AS refusal,
       SUM(at >= UNIX_TIMESTAMP() - 3600) AS n_1h, COUNT(*) AS n_24h, MAX(at) AS last_at
  FROM event
 WHERE machine = 'bead' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 86400 AND from_state <> to_state
 GROUP BY from_state, to_state, event
UNION ALL
SELECT 'refused', from_state, NULL, event, refusal,
       SUM(at >= UNIX_TIMESTAMP() - 3600), COUNT(*), MAX(at)
  FROM event
 WHERE machine = 'bead' AND applied = 0 AND at >= UNIX_TIMESTAMP() - 86400
 GROUP BY from_state, event, refusal;
