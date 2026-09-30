#!/usr/bin/env bash
#
# test-legacy-state-checks-ungated.sh — CHECK5 (sentinel) and the three groomer STATE sweeps
# (detect_landed_but_open, detect_closed_unlanded_states, detect_false_blockers) detect a bug
# class spira_lifecycle does not yet cover: bd's own status/closed fields drifting from what
# actually landed, written by bash glue the lifecycle machine does not own.
#
# Deleting or gating these on lifecycle_enforce removes real detection with nothing to
# replace it until an ON-path equivalent exists. This is the trap that makes that a visible,
# deliberate act instead of a silent regression at the next lifecycle_enforce flip.
#
# defect: sp-pswer.1
# tier: T1
# covers: sentinel/src/pass.rs sentinel/src/check5.rs spira/lib.sh spira/groomer.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-legacy-state-checks-ungated.sh"

ROOT="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

lc() { tr '[:upper:]' '[:lower:]' <<< "$1"; }

audit_body()  { awk '/^    fn audit\(/{f=1} f{print} f && /^    }$/{exit}' "$1"; }
sweep_body()  { awk '/^  sweep\)$/{f=1} f{print} f && /^  ;;$/{exit}' "$1"; }

# ---------------------------------------------------------------------------------------
# CHECK5: check5() still exists, and audit()'s call to it carries no lifecycle_enforce gate.
# ---------------------------------------------------------------------------------------
want "check5.rs still defines check5" "pub fn check5" "$(cat "$ROOT/sentinel/src/check5.rs")"

body="$(audit_body "$ROOT/sentinel/src/pass.rs")"
want   "audit() still calls check5"               "check5(snap)" "$body"
nowant "and does not gate it on lifecycle_enforce" "lifecycle_enforce" "$(lc "$body")"

# SEEN RED: the same extraction, against a scratch copy with the call gated, must catch it —
# the assertion above is a real trap, not a tautology that would pass on any input.
gated="$TMP/pass.rs"
sed 's/self\.check5(snap);/if self.cfg.lifecycle_enforce { self.check5(snap); }/' \
    "$ROOT/sentinel/src/pass.rs" > "$gated"
want "SEEN RED: a lifecycle_enforce-gated call is caught" \
    "lifecycle_enforce" "$(lc "$(audit_body "$gated")")"

# ---------------------------------------------------------------------------------------
# GROOMER: the three STATE sweep functions still exist in lib.sh, and groomer.sh's sweep)
# branch still calls all three with no lifecycle_enforce gate.
# ---------------------------------------------------------------------------------------
for fn in detect_landed_but_open detect_closed_unlanded_states detect_false_blockers; do
    want "lib.sh still defines $fn" "${fn}()" "$(grep -m1 "^${fn}()" "$ROOT/spira/lib.sh")"
done

sbody="$(sweep_body "$ROOT/spira/groomer.sh")"
for fn in detect_landed_but_open detect_closed_unlanded_states detect_false_blockers; do
    want "groomer.sh sweep still calls $fn" "$fn" "$sbody"
done
nowant "and does not gate the sweep on lifecycle_enforce" "lifecycle_enforce" "$(lc "$sbody")"

# SEEN RED for the groomer side too.
gated_groomer="$TMP/groomer.sh"
sed 's/_sw_lbo="\$(detect_landed_but_open 2>\/dev\/null)"/if [ "${SPIRA_LIFECYCLE_ENFORCE:-0}" != 1 ]; then _sw_lbo="$(detect_landed_but_open 2>\/dev\/null)"; fi/' \
    "$ROOT/spira/groomer.sh" > "$gated_groomer"
want "SEEN RED: a lifecycle_enforce-gated sweep call is caught" \
    "lifecycle_enforce" "$(lc "$(sweep_body "$gated_groomer")")"

tl_summary
