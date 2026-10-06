#!/usr/bin/env bash
#
# test-strand-truncated.sh — a sentinel pass truncated before evaluating a partition is
#   reported as 'pass-truncated' (info) by strand.sh, not as 'starved' (escalate).
#
#   ./test-strand-truncated.sh
#
# WHY THIS EXISTS. sp-ow8n: when bd calls are slow under lock contention, a sentinel pass
# may not reach every declared partition within its allotted time. The partitions it skips
# are absent from the log — identical to "nothing ready" from strand.sh's perspective.
# strand.sh then reports 'starved' and points at the sentinel timer, which is healthy.
#
# The fix is two parts:
#   sentinel.sh:  log "CHECK7 $f: not evaluated (pass budget exhausted)" when the budget
#                 runs out before a fayth is evaluated.
#   strand.sh:    detect that line in the most recent pass and emit 'pass-truncated' (info)
#                 instead of 'starved' (escalate).
#
# THREE CASES (law-absence-needs-a-positive-control). The classifier's own disposition
# once PASS_TRUNCATED is set (the positive control and the info-row assertion) moved to
# test-strand-classify.sh, which shares one positive control across every deliberate-state
# suppression instead of re-proving it per file. This suite keeps the pass-budget coverage
# that only it has:
#
#   2. STRAND.SH DETECTION — sentinel log contains "not evaluated" for a fayth working
#      this partition → strand.sh report emits "pass-truncated", not "starved".
#
#   3. NO TRUNCATION IN LOG — sentinel log exists but has no "not evaluated" line →
#      strand.sh report emits "starved" as before.
#
#   4. SENTINEL BUDGET — with SPIRA_SENTINEL_PASS_BUDGET_SECS=0 and two task fayths,
#      sentinel.sh logs "not evaluated (pass budget exhausted)" for both.
#
# defect: sp-ow8n
# tier: T1
# covers: strand/src/* sentinel/src/* spira/lib.sh UC-ops-detection-remediation-22
# hermetic-ok: cases 2-3 use no database; case 4 uses no database (SPIRA_SKIP_RECLAIM=1)
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run"

# THE STRAND AND SENTINEL ARE BINARIES (strand.sh, sentinel.sh are gone), resolved as conf.sh's
# the tree's build on PATH provides them. Both source lib.sh from SPIRA_HOME, so each stub
# home carries the real lib.sh and what it sources.
_libs() { local _s; for _s in lib.sh conf.sh suite-covers.sh conf.d; do ln -sfn "$HERE/$_s" "$1/$_s"; done; }

echo "test-strand-truncated.sh"

# ======================================================================================
echo
echo "case 2 — strand detection: sentinel log 'not evaluated' → pass-truncated:"
# ======================================================================================
# Build a stub chamber with a fayth whose partition is "spira,test-plan". The sentinel
# log records that this fayth was not evaluated. Strand.sh detects it and passes
# PASS_TRUNCATED=1 to the classifier.
mkdir -p "$TMP/stubs/chamber"
_libs "$TMP/stubs"
# SPIRA_CHAMBER no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture chamber explicitly.
tl_config SPIRA_CHAMBER="$TMP/stubs/chamber"
cat > "$TMP/stubs/chamber/test-watcher.fayth" <<'FAYTH'
FAYTH_NAME=test-watcher
FAYTH_LABELS=spira,test-plan
FAYTH_EXCLUDE_LABELS=spira-poison,needs-operator
FAYTH_MAX_CONCURRENT=1
FAYTH_TIMEOUT_SECONDS=3600
FAYTH

# Mock bd returns the fixture bead for any list query (strand's store read, spira-claim's
# content read, the lifecycle stand-in's own read). bdq() prepends -C <db> to every call;
# strip it before matching the subcommand.
cat > "$TMP/mock-bd" <<'MOCKBD'
#!/usr/bin/env bash
case "${1:-}" in -C) shift 2 ;; esac
case "${1:-}" in
    list)  printf '[{"id":"sp-t1","title":"test bead","status":"open","issue_type":"task","labels":["spira","test-plan"]}]\n' ;;
    *)     exit 0 ;;
esac
MOCKBD
chmod +x "$TMP/mock-bd"
# strand's ready set is spira-claim's, over the lifecycle machine's READY rows (sp-7g5q6;
# sp-v62vn: the only mode) — not a bd ready query. The stand-in (testlib lc_mirror_bd)
# answers spira-lc `list` from the mock bd's store, so the open bead is a READY row; it goes
# first on PATH, where spira-claim finds spira-lc by name.
lc_mirror_bd "$TMP/lc"

# Sentinel log: one completed pass (state: open= line) followed by a "not evaluated"
# line for test-watcher in that same pass.
cat > "$TMP/run/sentinel.log" <<'LOG'
2026-01-01T00:00:00Z spira: state: open=1 plan_ready=1 in_progress=0 aeons=0
2026-01-01T00:00:01Z spira: CHECK7 test-watcher: not evaluated (pass budget exhausted)
LOG

tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/no-db" SPIRA_BD="$TMP/mock-bd"
out="$(
    SPIRA_HOME="$TMP/stubs" PATH="$TMP/stubs:$TMP/lc:$PATH" \
    SPIRA_DB="$TMP/no-db" SPIRA_BD="$TMP/mock-bd" \
    SPIRA_SUMMON=stub \
    SPIRA_LABELS="spira,test-plan" \
        strand report 2>/dev/null
)"
want   "detection: pass-truncated IS reported" "pass-truncated" "$out"
nowant "detection: starved IS NOT reported"    "starved"        "$out"

# ======================================================================================
echo
echo "case 3 — no truncation in log: strand still reports starved:"
# ======================================================================================
# Same setup but sentinel log has no "not evaluated" line for test-watcher.
cat > "$TMP/run/sentinel.log" <<'LOG'
2026-01-01T00:00:00Z spira: state: open=1 plan_ready=1 in_progress=0 aeons=0
2026-01-01T00:00:01Z spira: CHECK7 test-watcher: nothing ready in its partition
LOG

tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/no-db" SPIRA_BD="$TMP/mock-bd"
out="$(
    SPIRA_HOME="$TMP/stubs" PATH="$TMP/stubs:$TMP/lc:$PATH" \
    SPIRA_DB="$TMP/no-db" SPIRA_BD="$TMP/mock-bd" \
    SPIRA_SUMMON=stub \
    SPIRA_LABELS="spira,test-plan" \
        strand report 2>/dev/null
)"
want   "no truncation: starved IS raised"         "starved"        "$out"
nowant "no truncation: pass-truncated not raised" "pass-truncated" "$out"

# ======================================================================================
echo
echo "case 4 — sentinel budget: SPIRA_SENTINEL_PASS_BUDGET_SECS=0 logs both fayths as not evaluated:"
# ======================================================================================
# Sentinel runs with two task fayths and a zero budget. Both fayths should be logged
# as "not evaluated (pass budget exhausted)" without calling summon_fayth for either.
mkdir -p "$TMP/stubs2/chamber"
for _f in alpha beta; do
    cat > "$TMP/stubs2/chamber/$_f.fayth" <<FAYTH2
FAYTH_NAME=$_f
FAYTH_LABELS=spira,test-$_f
FAYTH_EXCLUDE_LABELS=spira-poison,needs-operator
FAYTH_MAX_CONCURRENT=1
FAYTH_TIMEOUT_SECONDS=3600
FAYTH2
done
# Stubs: pilgrimage, sending, reflect all exit 0 with no output.
for _s in pilgrimage.sh sending reflect.sh; do
    printf '#!/bin/sh\n' > "$TMP/stubs2/$_s"; chmod +x "$TMP/stubs2/$_s"
done
# strand ALSO backs lib.sh's aeon_alive/aeon_count/aeons_live_total/aeons_live_lanes shims
# now (wave 4.23, sp-0ffox) — a bare no-op here would make `$(aeon_count ...)` inside
# lib.sh's pass-start summary print nothing, not "0", and blow up the arithmetic that adds
# it (this case has two task fayths, alpha and beta, so that summary line IS computed).
# A plain no-op is still correct for strand's OWN verbs (report/check/throttle-state).
cat > "$TMP/stubs2/strand" <<'STRANDSTUB'
#!/bin/sh
case "$1" in
    aeon-alive) exit 1 ;;
    aeon-count|aeons-live-total|aeons-live-lanes) printf '0' ;;
esac
STRANDSTUB
chmod +x "$TMP/stubs2/strand"
_libs "$TMP/stubs2"
# systemctl stub: says unit is inactive so the landing dispatch path runs cleanly.
printf '#!/bin/sh\necho inactive\n' > "$TMP/stubs2/mock-systemctl"; chmod +x "$TMP/stubs2/mock-systemctl"
# launch/summon/notify stubs.
for _s in mock-launch mock-summon mock-notify; do
    printf '#!/bin/sh\nexit 0\n' > "$TMP/stubs2/$_s"; chmod +x "$TMP/stubs2/$_s"
done
touch "$TMP/stubs2/repo-map"

_run2="$TMP/run2"; mkdir -p "$_run2"
# SPIRA_CHAMBER no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture chamber explicitly.
tl_config SPIRA_RUN="$_run2" SPIRA_DB="$TMP/no-db" SPIRA_FAYTHS="alpha beta" \
    SPIRA_NOTIFY="$TMP/stubs2/mock-notify" SPIRA_CHAMBER="$TMP/stubs2/chamber" \
    SPIRA_BD="$TMP/mock-bd"
out="$(env -i \
    PATH="$PATH" HOME="$HOME" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_HOME="$TMP/stubs2" PATH="$TMP/stubs2:$PATH" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_SENTINEL_PASS_BUDGET_SECS=0 \
    SPIRA_LAND_STALE=999999 \
    SPIRA_SYSTEMCTL="$TMP/stubs2/mock-systemctl" \
    SPIRA_LAUNCH="$TMP/stubs2/mock-launch" \
    SPIRA_SUMMON="$TMP/stubs2/mock-summon" \
    PATH="$TMP/stubs2:$PATH" \
        sentinel 2>&1)"

want   "alpha logged as not evaluated" "CHECK7 alpha: not evaluated (pass budget exhausted)" "$out"
want   "beta logged as not evaluated"  "CHECK7 beta: not evaluated (pass budget exhausted)"  "$out"
nowant "alpha NOT logged as summoned"  "summoned a alpha"                                    "$out"
nowant "beta NOT logged as summoned"   "summoned a beta"                                     "$out"
tl_summary
