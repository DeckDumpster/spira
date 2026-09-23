#!/usr/bin/env bash
#
# test-certify-suites-off.sh — SPIRA_CERTIFY_SUITES=off makes queue-mode certification run the
#   gate's fences and none of its suites, and a fences-only pass never reads back as a full one.
#
# THE CASE (2026-09-23). Certification ran every closed bead's suites on this box before it
# could join a batch, and the batch's CI run then ran them again. In 48 hours thirty closed
# beads went RED here (thirteen `timeout`) and none reached a batch.
#
# PROPERTIES
#   1. gate-touched.sh selects NOTHING when SPIRA_GATE_SUITES=off, and the gate command's own
#      `[ -n "$_s" ] || exit 0` then passes. POSITIVE CONTROL: the same call with the mode on
#      selects the ejected suite, so the empty answer is the switch and not a broken selector.
#   2. gate.sh's cache key differs between suites=on and suites=off for the same tree, so a
#      fences-only PASS cannot satisfy a later full gate. POSITIVE CONTROL: the same mode twice
#      yields the same key, so the difference is the mode and not noise.
#   3. landing.sh passes the switch on both queue-mode certification calls, and not on the
#      push-mode call, which lands straight on the base and must keep its suites.
#
# covers: spira/gate-touched.sh spira/gate.sh spira/landing.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-certify-suites-off.sh"
cd "$ROOT" || { bad "cd to repo root" "$ROOT"; exit 1; }
HEAD_SHA="$(git rev-parse HEAD 2>/dev/null)"

echo "1. gate-touched.sh:"
sel_on="$(SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash spira/gate-touched.sh "$HEAD_SHA" "$HEAD_SHA" 2>/dev/null)"
is "positive control: suites on selects the ejected suite" "test-conf.sh" "$sel_on"
sel_off="$(SPIRA_GATE_SUITES=off SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash spira/gate-touched.sh "$HEAD_SHA" "$HEAD_SHA" 2>/dev/null)"
is "suites off selects nothing, not even an ejected suite" "" "$sel_off"
# The repo-map gate command's shape: an empty selection exits 0 before any suite runs.
cmd_rc() { SPIRA_GATE_SUITES="$1" SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash -c '
    _s="$(bash spira/gate-touched.sh "$0" "$0")"; [ -n "$_s" ] || exit 0; exit 9' "$HEAD_SHA" 2>/dev/null; echo $?; }
is "gate command passes after the fences when suites are off" "0" "$(cmd_rc off)"
is "positive control: gate command reaches the suites when on" "9" "$(cmd_rc on)"

echo "2. gate.sh cache key:"
fn="$(awk '/^gate_key\(\) \{/{f=1} f{print} f&&/^\}/{exit}' spira/gate.sh)"
if [ -z "$fn" ]; then
    bad "gate_key located (positive control)" "awk extracted nothing"
else
    ok "gate_key located (positive control)"
    key() { ( eval "$fn"; REPO="$ROOT"; BR=HEAD; files="spira/gate.sh"; CMD="bash x"
              EXCLUDE="$ROOT/spira/gate.sh"; SKEW="$ROOT/spira/gate.sh"; REPO_NAME=spira
              SPIRA_GATE_SUITES="$1" gate_key ); }
    k_on="$(key on)"; k_on2="$(key on)"; k_off="$(key off)"
    [ -n "$k_on" ] && ok "key computed" || bad "key computed" "empty"
    is "positive control: same mode, same key" "$k_on" "$k_on2"
    [ "$k_on" != "$k_off" ] && ok "suites=off and suites=on have different keys" \
        || bad "suites=off and suites=on have different keys" "both $k_on"
fi

echo "3. landing.sh call sites:"
n_cert="$(grep -c 'SPIRA_GATE_SUITES="${SPIRA_CERTIFY_SUITES:-on}"' spira/landing.sh)"
is "both queue-mode certification calls pass the switch" "2" "$n_cert"
n_gate="$(grep -c '"$SPIRA_HOME/gate.sh"' spira/landing.sh)"
is "positive control: landing.sh has three gate.sh calls" "3" "$n_gate"

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
