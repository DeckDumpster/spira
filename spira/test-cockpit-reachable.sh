#!/usr/bin/env bash
#
# test-cockpit-reachable.sh — SP_REACHABLE and SP_STRANDED walk the dependency graph.
#
# WHAT THIS SUITE TESTS:
#
#   Fixture graph (6 open beads):
#     sp-a: open, no deps                  → seed (ready)
#     sp-b: open, depends on sp-a          → reachable (sp-a in seeds)
#     sp-c: open, depends on sp-b          → reachable (sp-b reachable)
#     sp-d: open, no deps                  → seed (ready)
#     sp-e: open, depends on sp-d, needs-ryan label → excluded from work universe
#     sp-f: open, depends on sp-e          → stranded (blocked by ask bead sp-e)
#
#   Expected: SP_REACHABLE=4 (sp-a, sp-b, sp-c, sp-d), SP_STRANDED=1 (sp-f).
#   sp-e is an ask bead — outside the work-only universe, but still blocks sp-f.
#   N (ready seeds) = 2 (sp-a, sp-d). M=4, K=2.
#
#   Also tests: SP_REACHABLE=? and SP_STRANDED=? when the store is unreadable.
#   And: health.sh renders "N ready · M reachable" and "K stranded" when K > 0.
#
# covers: spira/cockpit.sh cockpit/health.sh spira/collect.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

# Run just the reachable_keys probe against a given mock bd binary.
run_reachable() {
    local bd_path="$1"
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=builder \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SCOPE_LABEL=spira \
        SPIRA_BD="$bd_path" \
        bash "$HERE/cockpit.sh" reachable 2>/dev/null
}

# Like run_reachable but passes SPIRA_SCOPE_LABEL so the scope filter is active.
run_reachable_scoped() {
    local bd_path="$1" scope="$2"
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=builder \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SCOPE_LABEL="$scope" \
        SPIRA_BD="$bd_path" \
        bash "$HERE/cockpit.sh" reachable 2>/dev/null
}

# =============================================================================
# POSITIVE CONTROL — a bd that returns the fixture graph.
# A probe that always returns 0 would pass the refusal test but fail here.
# =============================================================================
echo "positive control: fixture graph → exact M and K"

BD_FIXTURE="$TMP/bd-fixture"
cat > "$BD_FIXTURE" <<'EOF'
#!/usr/bin/env bash
# Fixture: sp-a→sp-b→sp-c chain; sp-d→sp-e(needs-ryan)→sp-f chain.
printf '[
  {"id":"sp-a","status":"open","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-b","status":"open","labels":["spira","plan"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-b","depends_on_id":"sp-a","type":"blocks"}]},
  {"id":"sp-c","status":"open","labels":["spira","plan"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-c","depends_on_id":"sp-b","type":"blocks"}]},
  {"id":"sp-d","status":"open","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-e","status":"open","labels":["spira","plan","needs-ryan"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-e","depends_on_id":"sp-d","type":"blocks"}]},
  {"id":"sp-f","status":"open","labels":["spira","plan"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-f","depends_on_id":"sp-e","type":"blocks"}]}
]'
EOF
chmod +x "$BD_FIXTURE"

fixture_out="$(run_reachable "$BD_FIXTURE")"
# sp-a, sp-b, sp-c, sp-d reachable. sp-f stranded (blocked by ask sp-e). sp-e excluded.
is "SP_REACHABLE=4" "4" \
   "$(printf '%s\n' "$fixture_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"
is "SP_STRANDED=1" "1" \
   "$(printf '%s\n' "$fixture_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"

# =============================================================================
# NEGATIVE CONTROL — empty graph: M=0, K=0. Not ?.
# A probe that returns ? on an empty database is broken.
# =============================================================================
echo ""
echo "negative control: empty graph → 0, not ?"

BD_EMPTY="$TMP/bd-empty"
cat > "$BD_EMPTY" <<'EOF'
#!/usr/bin/env bash
printf '[]'
EOF
chmod +x "$BD_EMPTY"

empty_out="$(run_reachable "$BD_EMPTY")"
is "SP_REACHABLE=0 on empty graph" "0" \
   "$(printf '%s\n' "$empty_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"
is "SP_STRANDED=0 on empty graph" "0" \
   "$(printf '%s\n' "$empty_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"

# =============================================================================
# REFUSAL — bd refuses (schema mismatch, prints error, exits 0): renders ?.
# The reassuring answer must never be produced by a broken probe.
# =============================================================================
echo ""
echo "refusal: bd refuses → ?"

BD_REFUSED="$TMP/bd-refused"
cat > "$BD_REFUSED" <<'EOF'
#!/usr/bin/env bash
echo "schema version mismatch: database is at v61, binary knows up to v53"
exit 0
EOF
chmod +x "$BD_REFUSED"

ref_out="$(run_reachable "$BD_REFUSED")"
is "SP_REACHABLE=? on refusal" "?" \
   "$(printf '%s\n' "$ref_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"
is "SP_STRANDED=? on refusal" "?" \
   "$(printf '%s\n' "$ref_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"

# =============================================================================
# POISON STOPPER — a spira-poison bead blocks its downstream, same as needs-ryan.
# =============================================================================
echo ""
echo "poison stopper: spira-poison bead and downstream are stranded"

BD_POISON="$TMP/bd-poison"
cat > "$BD_POISON" <<'EOF'
#!/usr/bin/env bash
# sp-x: ready; sp-y: depends on sp-x, has spira-poison; sp-z: depends on sp-y
printf '[
  {"id":"sp-x","status":"open","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-y","status":"open","labels":["spira","plan","spira-poison"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-y","depends_on_id":"sp-x","type":"blocks"}]},
  {"id":"sp-z","status":"open","labels":["spira","plan"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-z","depends_on_id":"sp-y","type":"blocks"}]}
]'
EOF
chmod +x "$BD_POISON"

poison_out="$(run_reachable "$BD_POISON")"
# sp-x: reachable (seed). sp-y: stopper. sp-z: downstream of stopper → not reachable.
is "SP_REACHABLE=1 with poison stopper" "1" \
   "$(printf '%s\n' "$poison_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"
is "SP_STRANDED=2 with poison stopper" "2" \
   "$(printf '%s\n' "$poison_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"

# =============================================================================
# health.sh rendering — "N ready · M reachable" and "K stranded" when K > 0.
# Screenshot: this output is the pane row as the operator sees it.
# =============================================================================
echo ""
echo "health.sh: NEXT row shows N ready · M reachable · K stranded"

SNAP="$TMP/cockpit.env"
BUDGET="$TMP/budget.env"
touch "$BUDGET"
{
    printf 'SP_AT=%s\n' "$(date +%s)"
    printf 'SP_READY=2\n'
    printf 'SP_REACHABLE=5\n'
    printf 'SP_STRANDED=3\n'
    printf 'SP_NEXT_N=2\n'
    printf 'SP_NEXT0=P1 builder sp-a a ready bead\n'
    printf 'SP_NEXT1=P2 builder sp-d another ready bead\n'
    # Minimal keys for health.sh to render without crashing
    printf 'SP_AEONS=0\nSP_SENTINEL_AGE=5\nSP_OPS_AGE=5\nSP_AURON_AGE=5\n'
    printf 'SP_SENTINEL_TIMER=1\nSP_OPS_TIMER=1\nSP_AURON_TIMER=1\nSP_AURON_FIRING=0\n'
    printf 'SP_TOK_WIN=0\nSP_TOK_WINDOW_H=5\n'
    printf 'SP_RATELIM_5H_PCT=0\nSP_RATELIM_7D_PCT=0\nSP_RATELIM_5H_MIN=0\nSP_RATELIM_7D_MIN=0\n'
    printf 'SP_RATELIM_5H_ETA=-\nSP_RATELIM_7D_ETA=-\nSP_RATELIM_AGE=0\n'
} > "$SNAP"

pane_out="$(SPIRA_RUN="$TMP" bash "$HERE/../cockpit/health.sh" once 2>/dev/null)"
echo "--- pane NEXT row (screenshot) ---"
printf '%s\n' "$pane_out" | grep -i 'NEXT\|ready\|reachable' || true
echo "---"
want "NEXT label in pane"       "NEXT"       "$pane_out"
want "ready count in pane"      "2 ready"    "$pane_out"
want "reachable count in pane"  "reachable"  "$pane_out"
want "stranded count in pane"   "stranded"   "$pane_out"

# =============================================================================
# health.sh: K=0 suppresses the stranded clause.
# =============================================================================
echo ""
echo "health.sh: K=0 → no stranded clause"

{
    printf 'SP_AT=%s\n' "$(date +%s)"
    printf 'SP_READY=3\n'
    printf 'SP_REACHABLE=7\n'
    printf 'SP_STRANDED=0\n'
    printf 'SP_NEXT_N=3\n'
    printf 'SP_AEONS=0\nSP_SENTINEL_AGE=5\nSP_OPS_AGE=5\nSP_AURON_AGE=5\n'
    printf 'SP_SENTINEL_TIMER=1\nSP_OPS_TIMER=1\nSP_AURON_TIMER=1\nSP_AURON_FIRING=0\n'
    printf 'SP_TOK_WIN=0\nSP_TOK_WINDOW_H=5\n'
    printf 'SP_RATELIM_5H_PCT=0\nSP_RATELIM_7D_PCT=0\nSP_RATELIM_5H_MIN=0\nSP_RATELIM_7D_MIN=0\n'
    printf 'SP_RATELIM_5H_ETA=-\nSP_RATELIM_7D_ETA=-\nSP_RATELIM_AGE=0\n'
} > "$SNAP"

pane_no_strand="$(SPIRA_RUN="$TMP" bash "$HERE/../cockpit/health.sh" once 2>/dev/null)"
want   "reachable still shown when K=0" "reachable"  "$pane_no_strand"
nowant "stranded absent when K=0"       "stranded"   "$pane_no_strand"

# =============================================================================
# health.sh: SP_REACHABLE=? renders ? not 0 (failed probe must not show all-clear).
# =============================================================================
echo ""
echo "health.sh: SP_REACHABLE=? renders ? not 0"

{
    printf 'SP_AT=%s\n' "$(date +%s)"
    printf 'SP_READY=2\n'
    printf 'SP_REACHABLE=?\n'
    printf 'SP_STRANDED=?\n'
    printf 'SP_NEXT_N=2\n'
    printf 'SP_AEONS=0\nSP_SENTINEL_AGE=5\nSP_OPS_AGE=5\nSP_AURON_AGE=5\n'
    printf 'SP_SENTINEL_TIMER=1\nSP_OPS_TIMER=1\nSP_AURON_TIMER=1\nSP_AURON_FIRING=0\n'
    printf 'SP_TOK_WIN=0\nSP_TOK_WINDOW_H=5\n'
    printf 'SP_RATELIM_5H_PCT=0\nSP_RATELIM_7D_PCT=0\nSP_RATELIM_5H_MIN=0\nSP_RATELIM_7D_MIN=0\n'
    printf 'SP_RATELIM_5H_ETA=-\nSP_RATELIM_7D_ETA=-\nSP_RATELIM_AGE=0\n'
} > "$SNAP"

pane_q="$(SPIRA_RUN="$TMP" bash "$HERE/../cockpit/health.sh" once 2>/dev/null)"
want   "? reachable appears in pane" "? reachable" "$pane_q"
nowant "0 reachable must not appear" "0 reachable" "$pane_q"

# =============================================================================
# ASK + INSIGHT EXCLUSION — the acceptance fixture from sp-7gq94.
# 3 work beads (one poisoned), 2 ask beads (needs-ryan), 1 insight bead.
# After fix: stranded = 1 (the poisoned work bead only).
# Positive control (law-a-regression-test-must-be-seen-to-fail): with the old
# code (ask beads and insights counted in universe) this fixture gives stranded=3
# (poison + 2 asks). That confirms the fixture would catch a regression.
# =============================================================================
echo ""
echo "ask/insight exclusion: acceptance fixture (sp-7gq94) → stranded=1"

BD_ASKS="$TMP/bd-asks"
cat > "$BD_ASKS" <<'EOF'
#!/usr/bin/env bash
# 3 work beads (sp-w1 poisoned, sp-w2/sp-w3 plain), 2 asks, 1 insight.
printf '[
  {"id":"sp-w1","status":"open","labels":["spira","plan","spira-poison"],"issue_type":"task"},
  {"id":"sp-w2","status":"open","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-w3","status":"open","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-a1","status":"open","labels":["spira","plan","needs-ryan"],"issue_type":"task"},
  {"id":"sp-a2","status":"open","labels":["spira","plan","needs-ryan"],"issue_type":"task"},
  {"id":"sp-i1","status":"open","labels":["spira","plan","insight"],"issue_type":"task"}
]'
EOF
chmod +x "$BD_ASKS"

asks_out="$(run_reachable "$BD_ASKS")"
# sp-w2 and sp-w3 reachable. sp-w1 (poison) stranded. Asks and insight excluded.
is "SP_REACHABLE=2 with ask+insight exclusion" "2" \
   "$(printf '%s\n' "$asks_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"
is "SP_STRANDED=1 with ask+insight exclusion" "1" \
   "$(printf '%s\n' "$asks_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"

# Screenshot: pane renders "1 stranded" — only the poisoned work bead.
{
    printf 'SP_AT=%s\n' "$(date +%s)"
    printf 'SP_READY=2\n'
    printf 'SP_REACHABLE=2\n'
    printf 'SP_STRANDED=1\n'
    printf 'SP_NEXT_N=2\n'
    printf 'SP_NEXT0=P2 builder sp-w2 a work bead\n'
    printf 'SP_NEXT1=P2 builder sp-w3 another work bead\n'
    printf 'SP_AEONS=0\nSP_SENTINEL_AGE=5\nSP_OPS_AGE=5\nSP_AURON_AGE=5\n'
    printf 'SP_SENTINEL_TIMER=1\nSP_OPS_TIMER=1\nSP_AURON_TIMER=1\nSP_AURON_FIRING=0\n'
    printf 'SP_TOK_WIN=0\nSP_TOK_WINDOW_H=5\n'
    printf 'SP_RATELIM_5H_PCT=0\nSP_RATELIM_7D_PCT=0\nSP_RATELIM_5H_MIN=0\nSP_RATELIM_7D_MIN=0\n'
    printf 'SP_RATELIM_5H_ETA=-\nSP_RATELIM_7D_ETA=-\nSP_RATELIM_AGE=0\n'
} > "$SNAP"

pane_ask="$(SPIRA_RUN="$TMP" bash "$HERE/../cockpit/health.sh" once 2>/dev/null)"
echo "--- NEXT pane screenshot (ask/insight excluded) ---"
printf '%s\n' "$pane_ask" | grep -i 'NEXT\|ready\|reachable\|stranded' || true
echo "---"
want "stranded shows 1 (only poisoned work bead)" "1 stranded" "$pane_ask"

# =============================================================================
# RELATES-TO DOES NOT STRAND — the acceptance fixture from sp-dcmfd.
# A work bead with a relates-to link to an open ask is still reachable.
# Positive control: same bead with a blocks edge → SP_STRANDED=1.
# =============================================================================
echo ""
echo "relates-to link: not a blocker → SP_STRANDED=0"

BD_RELATES="$TMP/bd-relates"
cat > "$BD_RELATES" <<'EOF'
#!/usr/bin/env bash
# sp-work: ready work bead; sp-ask: open ask; relates-to link from work to ask.
printf '[
  {"id":"sp-work","status":"open","labels":["spira","plan"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-work","depends_on_id":"sp-ask","type":"relates-to"}]},
  {"id":"sp-ask","status":"open","labels":["spira","plan","needs-ryan"],"issue_type":"task"}
]'
EOF
chmod +x "$BD_RELATES"

relates_out="$(run_reachable "$BD_RELATES")"
is "SP_STRANDED=0 for relates-to link" "0" \
   "$(printf '%s\n' "$relates_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"
is "SP_REACHABLE=1 for relates-to link" "1" \
   "$(printf '%s\n' "$relates_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"

echo ""
echo "positive control: blocks edge onto ask → SP_STRANDED=1"

BD_BLOCKS_ASK="$TMP/bd-blocks-ask"
cat > "$BD_BLOCKS_ASK" <<'EOF'
#!/usr/bin/env bash
# sp-work: depends via blocks on sp-ask (open ask) → stranded.
printf '[
  {"id":"sp-work","status":"open","labels":["spira","plan"],"issue_type":"task",
   "dependencies":[{"issue_id":"sp-work","depends_on_id":"sp-ask","type":"blocks"}]},
  {"id":"sp-ask","status":"open","labels":["spira","plan","needs-ryan"],"issue_type":"task"}
]'
EOF
chmod +x "$BD_BLOCKS_ASK"

blocks_ask_out="$(run_reachable "$BD_BLOCKS_ASK")"
is "SP_STRANDED=1 for blocks edge onto ask" "1" \
   "$(printf '%s\n' "$blocks_ask_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"
is "SP_REACHABLE=0 for blocks edge onto ask" "0" \
   "$(printf '%s\n' "$blocks_ask_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"

# =============================================================================
# SCOPE FILTER — reachable_keys respects SPIRA_SCOPE_LABEL (sp-xrgae).
# A bead with no scope label is unclaimable; it must not be counted as reachable
# or stranded — it belongs to neither set. Only in-scope beads are counted.
# Positive control: an in-scope bead IS counted, so a filter that drops everything
# would still fail here.
# =============================================================================
echo ""
echo "scope filter: out-of-scope bead not counted; in-scope bead still counted"

BD_SCOPE="$TMP/bd-scope"
cat > "$BD_SCOPE" <<'EOF'
#!/usr/bin/env bash
# sp-in: has the scope label; sp-out: no scope label (empty labels).
printf '[
  {"id":"sp-in","status":"open","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-out","status":"open","labels":[],"issue_type":"task"}
]'
EOF
chmod +x "$BD_SCOPE"

scope_out="$(run_reachable_scoped "$BD_SCOPE" "spira")"
is "SP_REACHABLE=1 with scope filter (only in-scope bead)" "1" \
   "$(printf '%s\n' "$scope_out" | grep '^SP_REACHABLE=' | sed 's/^SP_REACHABLE=//')"
is "SP_STRANDED=0 with scope filter (out-of-scope excluded)" "0" \
   "$(printf '%s\n' "$scope_out" | grep '^SP_STRANDED=' | sed 's/^SP_STRANDED=//')"

# =============================================================================
echo ""
printf 'test-cockpit-reachable: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
