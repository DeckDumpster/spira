CREATE OR REPLACE VIEW inv_tip_matches_branch AS
SELECT seq, bead, lc_state, lc_tip, branch_tip FROM bead_state
WHERE lc_state IN ('SUBMITTED', 'CERTIFIED') AND lc_tip IS DISTINCT FROM branch_tip;

CREATE OR REPLACE VIEW inv_landstate_certified_needs_lifecycle AS
SELECT seq, bead, landstate, lc_state, lc_tip, branch_tip FROM bead_state
WHERE landstate = 'CERTIFIED' AND (lc_state IS DISTINCT FROM 'CERTIFIED' OR lc_tip IS DISTINCT FROM branch_tip);

CREATE OR REPLACE VIEW inv_landed_is_on_local_main AS
SELECT seq, bead, lc_state, landstate, branch_tip, on_local_main FROM bead_state
WHERE (lc_state = 'LANDED' OR landstate = 'LANDED') AND on_local_main IS NOT TRUE;

CREATE OR REPLACE VIEW inv_closed_is_terminal AS
SELECT seq, bead, bd_status, lc_state FROM bead_state
WHERE bd_status = 'closed' AND (lc_state IS NULL OR lc_state NOT IN ('LANDED', 'SUPERSEDED', 'DROPPED', 'DONE'));

CREATE OR REPLACE VIEW inv_complete_has_start AS
SELECT c.seq, c.actor, c.started FROM events c
WHERE c.kind = 'complete'
  AND NOT EXISTS (SELECT 1 FROM events s WHERE s.kind = 'start' AND s.actor = c.actor AND s.vtime = c.started AND s.seq < c.seq);

CREATE OR REPLACE VIEW inv_actor_does_not_overlap_itself AS
SELECT seq, actor, open_runs FROM (
  SELECT seq, actor, kind,
         sum(CASE kind WHEN 'start' THEN 1 WHEN 'complete' THEN -1 ELSE 0 END)
           OVER (PARTITION BY actor ORDER BY seq ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS open_runs
  FROM events WHERE kind IN ('start', 'complete'))
WHERE kind = 'start' AND coalesce(open_runs, 0) > 0;
