#!/usr/bin/env bash
#
# test-watchtower-throttle.sh — admission throttle in watchtower --throttle-check
#
# WHAT THIS SUITE IS FOR
# ----------------------
# The admission throttle (sp-h7zzx) uses two inputs — queue depth and drain rate — to
# decide between three outcomes: throttle (depth high + drain active), escalate-stall
# (depth high + drain zero), or clear (depth below threshold).
#
# THE CRITICAL PAIR (the whole bead in two cases):
#   depth >= threshold AND drain active  → stamp written, throttle-engaged incident filed
#   depth >= threshold AND drain zero    → NO stamp, stall-fault incident filed instead
# Getting the second case wrong (throttling a stall) delays repairs and is actively harmful.
#
# POSITIVE CONTROLS FIRST (law-a-regression-test-must-be-seen-to-fail): each absence
# assertion is preceded by a fixture that proves the detector can fire.
#
# tier: T1
# covers: spira/watchtower.sh spira/sentinel.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-watchtower-throttle.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
NOW="$(date +%s)"

# Git repo used by SPIRA_TC_REPO so the throttle-check's branch/ancestry filters run
# against a controlled fixture rather than the real harness checkout.
GIT_REPO="$TMP/git"
git init -q -b main "$GIT_REPO" 2>/dev/null || { git init -q "$GIT_REPO"; git -C "$GIT_REPO" checkout -q -b main 2>/dev/null; }
git -C "$GIT_REPO" config user.email t@t
git -C "$GIT_REPO" config user.name t
git -C "$GIT_REPO" commit -q --allow-empty -m init
GIT_BASE="$(git -C "$GIT_REPO" rev-parse HEAD)"

# Write a CERTIFIED landstate file AND create a branch with a commit NOT on main.
# Branch exists + tip not on main → throttle counts this as live queue depth.
certified() {
    local id="$1"
    git -C "$GIT_REPO" checkout -q --detach "$GIT_BASE" 2>/dev/null
    git -C "$GIT_REPO" commit -q --allow-empty -m "$id"
    local sha; sha="$(git -C "$GIT_REPO" rev-parse HEAD)"
    git -C "$GIT_REPO" branch -f "spira/$id" HEAD 2>/dev/null
    git -C "$GIT_REPO" checkout -q main 2>/dev/null
    printf 'CERTIFIED %s %s\n' "$sha" "$NOW" > "$TMP/run/landstate/$id"
}

# CERTIFIED landstate with tip ON main — ancestry check filters it (landed, stale record).
certified_landed() {
    local id="$1"
    git -C "$GIT_REPO" branch -f "spira/$id" "$GIT_BASE" 2>/dev/null
    printf 'CERTIFIED %s %s\n' "$GIT_BASE" "$NOW" > "$TMP/run/landstate/$id"
}

# CERTIFIED landstate with NO branch — branch-existence check filters it (dropped/superseded).
certified_gone() {
    local id="$1"
    printf 'CERTIFIED fakeshafakeshafakeshafakeshafakeshafake %s\n' "$NOW" \
        > "$TMP/run/landstate/$id"
}

# Write a LANDED landstate file with a given age in seconds.
landed() { printf 'LANDED fakeshafakeshafakeshafakeshafakeshafake %s\n' \
               "$(( NOW - ${2:-60} ))" > "$TMP/run/landstate/$1"; }

fresh() {
    rm -rf "$TMP/run"
    mkdir -p "$TMP/run/landstate"
    rm -f "$TMP/inc-subjects" "$TMP/inc-refs" "$TMP/inc-causes"
    while IFS= read -r _br; do
        git -C "$GIT_REPO" branch -D "$_br" 2>/dev/null || true
    done < <(git -C "$GIT_REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null)
}

# Run --throttle-check in an isolated environment.
# SPIRA_TC_REPO and SPIRA_TC_LAND_REF route the git branch/ancestry filters to the
# test fixture repo. Override either with VAR="" to disable git filtering (old behavior).
wt_tc() {   # wt_tc [VAR=val ...]
    local mock="$TMP/mock-inc.sh"
    cat > "$mock" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$2"                              >> "$INC_SUBJECTS"
printf '%s\n' "${SPIRA_INCIDENT_CAUSE:-}"       >> "$INC_CAUSES"
printf '%s\n' "${SPIRA_INCIDENT_REF:-}"         >> "$INC_REFS"
cat > /dev/null
MOCK
    chmod +x "$mock"
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_THROTTLE_STAMP="$TMP/run/queue-throttled" \
        SPIRA_INCIDENT_SH="$mock" \
        SPIRA_TC_REPO="$GIT_REPO" \
        SPIRA_TC_LAND_REF="refs/heads/main" \
        INC_SUBJECTS="$TMP/inc-subjects" \
        INC_CAUSES="$TMP/inc-causes" \
        INC_REFS="$TMP/inc-refs" \
        "$@" bash "$HERE/watchtower.sh" --throttle-check 2>/dev/null
}

stamp_exists() { [ -f "$TMP/run/queue-throttled" ] && echo yes || echo no; }
subjects()     { cat "$TMP/inc-subjects" 2>/dev/null || true; }
causes()       { cat "$TMP/inc-causes"   2>/dev/null || true; }

# ======================================================================================
echo
echo "positive controls — each detector fires on its fixture:"
# ======================================================================================
# POSITIVE CONTROL: throttle. Depth high, drain active → stamp written.
fresh
for i in $(seq 1 16); do certified "sp-tc-c${i}"; done    # 16 CERTIFIED (depth >= 16)
landed "sp-tc-l1" 60                                       # landed 1m ago (drain active)
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "throttle: stamp written when depth=16 >= threshold=16, drain=1m < 50m" \
     "yes" "$(stamp_exists)"
want "throttle: engage incident filed" "QUEUE THROTTLED" "$(subjects)"
want "throttle: cause is throttle-engaged" "throttle-engaged" "$(causes)"

# POSITIVE CONTROL: stall. Depth high, drain zero → NO stamp, stall incident filed.
fresh
# Add async gate commits to main (sp-c8w16, sp-74gwk) so stall detection proceeds
git -C "$GIT_REPO" commit -q --allow-empty -m "sp-c8w16 async gate implementation"
git -C "$GIT_REPO" commit -q --allow-empty -m "sp-74gwk async gate landing"
for i in $(seq 1 16); do certified "sp-tc-c${i}"; done    # 16 CERTIFIED
landed "sp-tc-l1" $(( 60 * 60 ))                          # landed 60m ago (stall)
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "stall: NO stamp when depth=16 >= threshold=16 but drain=60m >= 50m stall" \
     "no" "$(stamp_exists)"
want "stall: stall-fault incident filed" "deep+stalled" "$(subjects)"
want "stall: cause is throttle-stall" "throttle-stall" "$(causes)"

# ======================================================================================
echo
echo "THE CRITICAL PAIR — same depth, different drain rate:"
# ======================================================================================
# This is the whole bead in two assertions. Depth=16 in both cases.
#   drain active (1m)  → throttle stamp written
#   drain zero  (60m)  → no stamp, stall escalation instead
# Both verified above as positive controls. Now the absence assertions:

# Depth high + drain active = throttle (already verified above)
# Now verify: drain zero does NOT throttle (already verified as stall above)
# Extra verification: no stamp after stall case
fresh
# Add async gate commits to main so stall detection proceeds
git -C "$GIT_REPO" commit -q --allow-empty -m "sp-c8w16 async gate implementation"
git -C "$GIT_REPO" commit -q --allow-empty -m "sp-74gwk async gate landing"
for i in $(seq 1 16); do certified "sp-tc-c${i}"; done
landed "sp-tc-l1" $(( 55 * 60 ))   # 55m ago, above 50m stall threshold
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "critical pair: drain=55m does NOT throttle (stall treatment)" \
     "no" "$(stamp_exists)"
nowant "critical pair: no throttle-engaged incident for stall" \
       "QUEUE THROTTLED" "$(subjects)"

# ======================================================================================
echo
echo "depth below threshold — no action:"
# ======================================================================================
fresh
for i in $(seq 1 15); do certified "sp-tc-c${i}"; done    # 15 < 16 threshold
landed "sp-tc-l1" 60
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "depth=15 below threshold=16: no stamp" "no" "$(stamp_exists)"
is   "depth=15: no incident" "" "$(subjects)"

# ======================================================================================
echo
echo "hysteresis — lift when depth drops below release threshold:"
# ======================================================================================
# Stamp exists (throttle was engaged). Depth drops below RELEASE_AT → lift.
fresh
for i in $(seq 1 5); do certified "sp-tc-c${i}"; done     # depth=5, release_at=8
printf 'since=2026-09-17T20:15:00Z depth=16 since_land=3m\n' \
    > "$TMP/run/queue-throttled"                           # pre-existing stamp
landed "sp-tc-l1" 60
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_RELEASE_AT=8 \
      SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "lift: stamp removed when depth=5 < release_at=8" "no" "$(stamp_exists)"
want "lift: lift incident filed" "THROTTLE LIFTED" "$(subjects)"
want "lift: cause is throttle-lifted" "throttle-lifted" "$(causes)"

# Throttle-lifted case: depth between release and engage thresholds — maintain (do not lift)
fresh
for i in $(seq 1 10); do certified "sp-tc-c${i}"; done    # depth=10, between 8 and 16
printf 'since=2026-09-17T20:15:00Z depth=16 since_land=3m\n' \
    > "$TMP/run/queue-throttled"
landed "sp-tc-l1" 60
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_RELEASE_AT=8 \
      SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "maintain: stamp NOT removed when depth=10 between release=8 and engage=16" \
     "yes" "$(stamp_exists)"
is   "maintain: no new incident while holding" "" "$(subjects)"

# ======================================================================================
echo
echo "override off — throttle pinned disabled:"
# ======================================================================================
fresh
for i in $(seq 1 20); do certified "sp-tc-c${i}"; done    # depth well above threshold
landed "sp-tc-l1" 60                                       # drain active
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_OVERRIDE=off
is   "override=off: no stamp even when depth=20 > threshold=16" \
     "no" "$(stamp_exists)"
is   "override=off: no incident filed" "" "$(subjects)"

# Existing stamp is cleared when override=off
fresh
for i in $(seq 1 20); do certified "sp-tc-c${i}"; done
printf 'since=2026-09-17T20:15:00Z depth=16 since_land=3m\n' \
    > "$TMP/run/queue-throttled"
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_OVERRIDE=off
is   "override=off: existing stamp cleared" "no" "$(stamp_exists)"

# ======================================================================================
echo
echo "halted world — throttle check skipped:"
# ======================================================================================
fresh
for i in $(seq 1 20); do certified "sp-tc-c${i}"; done
landed "sp-tc-l1" 60
printf 'halted\n' > "$TMP/run/world.halted"
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16
is   "halted world: no stamp" "no" "$(stamp_exists)"
is   "halted world: no incident" "" "$(subjects)"

# Running world (no halt stamp) still throttles: positive control for halt guard
fresh
for i in $(seq 1 16); do certified "sp-tc-c${i}"; done
landed "sp-tc-l1" 60
# No world.halted stamp
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "running world: stamp written (positive control for halt guard)" \
     "yes" "$(stamp_exists)"

# ======================================================================================
echo
echo "no landings recorded — treated as stall (drain=?):"
# ======================================================================================
# No LANDED records means since_land="?", treated same as drain zero.
fresh
for i in $(seq 1 16); do certified "sp-tc-c${i}"; done    # depth high, no LANDED records
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "no LANDED records: no stamp (treated as stall)" "no" "$(stamp_exists)"

# ======================================================================================
echo
echo "engage escalation fires once per engagement (dedup via stable ref):"
# ======================================================================================
fresh
for i in $(seq 1 16); do certified "sp-tc-c${i}"; done
landed "sp-tc-l1" 60
rm -f "$TMP/inc-refs"
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=16 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
refs="$(cat "$TMP/inc-refs" 2>/dev/null || true)"
engage_refs="$(printf '%s\n' "$refs" | grep 'throttle-engaged' | sort -u | grep -c . 2>/dev/null || echo 0)"
is   "engage ref is stable: single unique ref across two calls" "1" "$engage_refs"

# ======================================================================================
echo
echo "stale CERTIFIED records: only live branches count toward depth:"
# ======================================================================================
# Fixture: 7 landed (branch tip on main), 1 rebased-landed (no branch, tip off main),
# 2 superseded (no branch), 1 genuinely live (branch exists, tip off main).
# Correct live depth with ancestry filter: 1. With branch-only filter: 2 (live + rebase).
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail): verify that even
# with stale records present (landed + gone branches), the filter correctly counts
# only the live branches. With ancestry filter disabled (SPIRA_TC_LAND_REF=""), the
# depth should count branches that exist but are not on main.
fresh
for _id in tc-land-1 tc-land-2 tc-land-3 tc-land-4 tc-land-5 tc-land-6 tc-land-7; do
    certified_landed "$_id"
done
certified_gone "tc-rebase-1"      # landed after rebase — tip not on main, branch gone
certified_gone "tc-super-1"       # superseded — no branch
certified_gone "tc-super-2"       # superseded — no branch
certified "tc-live-1"             # genuinely waiting — branch exists, tip not on main
landed "tc-prev-land" 30          # drain active (30s ago)
wt_tc SPIRA_TC_LAND_REF="" \
      SPIRA_QUEUE_THROTTLE_DEPTH_AT=1 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "stale-filter positive control: branch-filter only, landed branches excluded → depth>=1 → stamp" \
     "yes" "$(stamp_exists)"

# With git filters: stale records excluded → depth=1 < threshold=2 → no stamp.
rm -f "$TMP/run/queue-throttled"
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=2 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "11-record fixture: only live branch counts → depth=1 < threshold=2 → no stamp" \
     "no" "$(stamp_exists)"

# Verify the live branch DOES engage when above threshold (filter fires, not gate).
fresh
certified "tc-live-only"
landed "tc-prev-land" 30
wt_tc SPIRA_QUEUE_THROTTLE_DEPTH_AT=1 SPIRA_QUEUE_THROTTLE_STALL_MINS=50
is   "1 live cert at threshold=1 → stamp written (positive control for filter)" \
     "yes" "$(stamp_exists)"

# ======================================================================================
echo
echo "sentinel.sh CHECK7: throttle stamp gates task pool to 0 — real behaviour rows (G2):"
# ======================================================================================
# lib.sh functions, not a source grep: ck7_pool (pool minus task-live), ck7_throttled
# (stamp + override -> throttled flag) and ck7_fill_cap (per-persona fill cap, G3).
#
# Sourced in an isolated child process per call, never into this suite's own shell — this
# suite's main process must not inherit a real SPIRA_CONF/SPIRA_RUN (law-gates-run-in-a-
# clean-environment); wt_tc above already isolates the same way for a full watchtower run.
libcall() {   # libcall '<shell code calling one of the lib.sh functions above>'
    env -i PATH="$PATH" HOME="$TMP" SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/libcall-run" \
        bash -c '. "'"$HERE"'/lib.sh" >/dev/null 2>&1 || exit 1
'"$1"
}

# POSITIVE CONTROL: ck7_pool subtracts task-live from the configured pool.
is "ck7_pool: 4 max, 1 live -> 3 free" "3" "$(libcall 'ck7_pool 4 1')"
is "ck7_pool: task-live at or above max floors at 0" "0" "$(libcall 'ck7_pool 4 4')"
is "ck7_pool: no pool configured -> empty (today's behaviour unchanged)" \
   "" "$(libcall 'ck7_pool "" 0')"

# ck7_throttled: the stamp alone throttles; SPIRA_QUEUE_THROTTLE_OVERRIDE=off pins it clear.
is "ck7_throttled: stamp present, no override -> throttled" "1" "$(libcall 'ck7_throttled 1 ""')"
is "ck7_throttled: no stamp -> not throttled" "0" "$(libcall 'ck7_throttled 0 ""')"
is "ck7_throttled: stamp present but override=off -> NOT throttled" \
   "0" "$(libcall 'ck7_throttled 1 off')"

# check7_pool_decision composes with ck7_throttled: throttled pool holds at 0 unless an
# express bead is ready, in which case it grants exactly 1 (already covered in
# test-express-lane.sh; these rows are the override=off case reaching an unthrottled pool).
is "override=off: pool stays the ck7_pool value, untouched by check7_pool_decision" \
   "3" "$(libcall 'check7_pool_decision "$(ck7_throttled 1 off)" "$(ck7_pool 4 1)" 0')"
is "no override: stamp forces the same pool to 0 (no express bead ready)" \
   "0" "$(libcall 'check7_pool_decision "$(ck7_throttled 1 "")" "$(ck7_pool 4 1)" 0')"

# ck7_fill_cap: the per-persona fill cap that stops an unbounded summon loop on a host
# with no pool configured (G3 — this had no test at all before this extraction).
is "ck7_fill_cap: below the cap, no pool -> continue" \
   "continue" "$(libcall 'SPIRA_MAX_LIVE_AEONS=2 ck7_fill_cap 1 ""')"
is "ck7_fill_cap: at the cap, no pool -> stop" \
   "stop" "$(libcall 'SPIRA_MAX_LIVE_AEONS=2 ck7_fill_cap 2 ""')"
is "ck7_fill_cap: pool exhausted before the cap -> stop" \
   "stop" "$(libcall 'SPIRA_MAX_LIVE_AEONS=2 ck7_fill_cap 0 0')"
is "ck7_fill_cap: unset SPIRA_MAX_LIVE_AEONS defaults to 4" \
   "stop" "$(libcall 'ck7_fill_cap 4 ""')"

# Structural control retained: lanes must still precede the pool gate in sentinel.sh, since
# ck7_throttled/check7_pool_decision replacing the pool are only reached after the lane
# loop — a real behaviour row cannot see file ordering, so this stays a source check.
sentinel="$HERE/sentinel.sh"
lane_line="$(grep -n 'for f in \$LANE_FAYTHS' "$sentinel" 2>/dev/null | head -1 | cut -d: -f1 || echo 0)"
gate_line="$(grep -n 'ck7_throttled' "$sentinel" 2>/dev/null | head -1 | cut -d: -f1 || echo 0)"
if [ -n "$lane_line" ] && [ -n "$gate_line" ] && \
   [ "$lane_line" -gt 0 ] && [ "$gate_line" -gt 0 ] && \
   [ "$lane_line" -lt "$gate_line" ]; then
    ok "lanes loop (line ${lane_line}) precedes pool gate (line ${gate_line}): lanes unaffected"
else
    bad "lanes loop must precede pool gate" \
        "lane_line=${lane_line} gate_line=${gate_line}"
fi

tl_summary
