#!/usr/bin/env bash
#
# test-fayth.sh — summon_fayth uses each fayth's own predicate, not a shared one;
#   spira_fayths discovers every fayth in the chamber rather than returning a hardcoded name.
#
#   ./test-fayth.sh
#
# THE DEFECT. sentinel.sh CHECK 7 computed a single $ready from --label spira,plan — the
# builder's partition — and gated every persona's summon on it. Ops was therefore
# unreachable by construction: an incident arriving while no plan work was queued summoned
# nothing, and ops only ever woke when the BUILDER had work, which is exactly backwards for
# an on-call role. ops.fayth shipped complete and inert on 2026-09-05, with a correct
# predicate that nothing ever evaluated.
#
# THE FIX. summon_fayth calls fayth_ready, which sources the fayth file and asks ready_count
# through THAT fayth's FAYTH_LABELS — so each persona's readiness is its own question.
# spira_fayths enumerates chamber/*.fayth rather than returning a literal name, so adding a
# fayth file is the whole installation step.
#
# WHY summon_fayth IS IN lib.sh. Everything else in a sentinel pass touches the real
# repository and the real database; moving summon_fayth here is what makes it exercisable
# by this suite without either.
#
# WHAT THIS SUITE DOES NOT USE. No bd, no systemd, no network. ready_count is overridden
# with a stub (after sourcing lib.sh, so the subshell in fayth_ready inherits the
# override). The observed label is written to a file because a subshell variable assignment
# cannot reach the parent.
#
# defect: sp-fayth-predicate
# covers: spira/lib.sh spira/sentinel.sh spira/chamber/ops.fayth
# hermetic-ok: no database, no systemd; ready_count and SPIRA_SUMMON are stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# Run with the real chamber so the suite exercises the installed fayths, not a model of
# them. SPIRA_CONF at a nonexistent path so no host config leaks verdicts into the suite
# (law-gates-run-in-a-clean-environment). SPIRA_RUN at a temp dir so capacity_paused has
# no pause file and returns 1 (not paused).
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# Labels file: ready_count writes here from inside a subshell (fayth_ready's (...) call)
# so the parent can observe which predicate was asked. A subshell variable assignment
# cannot reach the parent, so the file is the channel.
LABELS_FILE="$T/observed-labels"

# Override ready_count AFTER sourcing lib.sh. bash subshells inherit the calling shell's
# functions, so fayth_ready — which invokes ready_count inside a (...) subshell — picks
# this override up rather than the real one. The label is written to LABELS_FILE, not to
# a variable, precisely because fayth_ready runs in a subshell.
MOCK_READY=0
ready_count() {
    printf '%s' "$1" > "$LABELS_FILE"
    printf '%d' "$MOCK_READY"
}

# Stub aeon_count so no running aeons are reported: fayth_free would otherwise see a
# live slot and return 0, blocking the summon path before ready_count is called.
aeon_count() { printf '0'; }

# Stub SPIRA_SUMMON so summon_fayth does not attempt to start a systemd unit.
MOCK_SUMMON="$T/summon"
printf '#!/bin/sh\nexit 0\n' > "$MOCK_SUMMON"
chmod +x "$MOCK_SUMMON"
export SPIRA_SUMMON="$MOCK_SUMMON"

echo "test-fayth.sh"

# ==========================================================================================
echo
echo "spira_fayths — discovers every fayth in the chamber, not just one"
# ==========================================================================================
# THE BUG. The default was a literal 'builder'; adding ops.fayth to the chamber made it
# present in the directory but absent from every roster, so it was never summoned and its
# correct predicate was never evaluated. Enumerating chamber/*.fayth is what makes
# installing a fayth the same thing as registering a persona.
unset SPIRA_FAYTHS 2>/dev/null || true
got="$(spira_fayths)"
# The real chamber has builder and ops at minimum; both must appear.
want "builder appears in roster" "builder" "$got"
want "ops appears in roster"     "ops"     "$got"

# ==========================================================================================
echo
echo "ops.fayth — its FAYTH_LABELS is its OWN partition, not the builder's"
# ==========================================================================================
# A POSITIVE CONTROL AGAINST THE REAL FILE. The defect was that ops.fayth carried a correct
# predicate that the harness never used; this assertion fails if the file is ever edited to
# carry the builder's predicate, or if fayth_get resolves the wrong file.
ops_labels="$(fayth_get ops FAYTH_LABELS)"
lack "ops FAYTH_LABELS is not the builder's spira,plan" "spira,plan" " $ops_labels "
want "ops FAYTH_LABELS contains its own partition"     "incident"   "$ops_labels"

# ==========================================================================================
echo
echo "summon_fayth — ops uses ITS OWN predicate, not the builder's"
# ==========================================================================================
MOCK_READY=1; rm -f "$LABELS_FILE"
summon_fayth ops >/dev/null 2>&1 || true
observed="$(cat "$LABELS_FILE" 2>/dev/null)"
lack "ops did NOT ask for spira,plan beads" "spira,plan" " $observed "
want "ops asked for its own incident beads" "incident"   "$observed"

# ==========================================================================================
echo
echo "summon_fayth — builder uses ITS OWN predicate"
# ==========================================================================================
MOCK_READY=1; rm -f "$LABELS_FILE"
summon_fayth builder >/dev/null 2>&1 || true
observed="$(cat "$LABELS_FILE" 2>/dev/null)"
is "builder asked for its own partition beads" \
   "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan}" "$observed"

# ==========================================================================================
echo
echo "positive control — old single-predicate approach misses ops work"
# ==========================================================================================
# THE OLD CODE IN THREE LINES:
#   ready="$(ready_count spira,plan ...)"   # hardcoded builder predicate
#   if [ "$ready" -gt 0 ]; then             # gates ALL personas, including ops
#       for f in $FAYTHS; do summon_fayth "$f"; done
#   fi
#
# Step 1: plan_ready=0. Under the old code, this blocked ALL summons.
MOCK_READY=0
plan_ready="$(ready_count "spira,plan" "")"
is "old code: plan_ready=0 (no plan work)" "0" "$plan_ready"

# Step 2: ops HAS work. Under the old code, it was never asked — summon_fayth
# was only reached when plan_ready>0. With the fix, summon_fayth asks ops's own
# predicate regardless of plan_ready.
MOCK_READY=1; rm -f "$LABELS_FILE"
summon_fayth ops >/dev/null 2>&1 || true
observed="$(cat "$LABELS_FILE" 2>/dev/null)"
# The key assertion: ops's predicate was asked DESPITE plan_ready=0.
want "ops predicate was asked even when plan_ready=0" "incident" "$observed"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
