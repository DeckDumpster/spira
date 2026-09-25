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
#   3. gate.sh's env -i allowlist carries the switch to the gate command.
#   4. landing.sh passes the switch on both queue-mode certification calls, and not on the
#      push-mode call, which lands straight on the base and must keep its suites.
#
# tier: T1
# covers: spira/gate-touched.sh spira/gate.sh spira/landing.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-certify-suites-off.sh"
# ITS OWN GIT REPOSITORY. The suite container is not a git checkout (its first run, on main
# gate 35934120122, found HEAD empty and every positive control failed), so the test builds
# the smallest repo the two scripts need: a commit holding spira/test-conf.sh, so an ejected
# suite resolves and the tree hash exists. The scripts under test still come from $HERE.
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
FIX="$TMP/repo"; mkdir -p "$FIX/spira"
printf '#!/usr/bin/env bash\n' > "$FIX/spira/test-conf.sh"
git -C "$FIX" init -q -b main && git -C "$FIX" add -A \
    && git -C "$FIX" -c user.name=t -c user.email=t@t commit -q -m fixture \
    || { bad "fixture repo built" "git init/commit failed"; exit 1; }
cd "$FIX" || { bad "cd to fixture repo" "$FIX"; exit 1; }
HEAD_SHA="$(git rev-parse HEAD 2>/dev/null)"
[ -n "$HEAD_SHA" ] && ok "fixture repo has a HEAD (positive control)" || bad "fixture repo has a HEAD" "empty"

echo "1. gate-touched.sh:"
sel_on="$(SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash "$HERE/gate-touched.sh" "$HEAD_SHA" "$HEAD_SHA" 2>/dev/null)"
# The positive control now checks that test-conf.sh is among the selected suites,
# not that it's the only one, because test-verdict-flow.sh also covers conf.sh
case "$sel_on" in *test-conf.sh*)
    ok "positive control: suites on selects the ejected suite"
    ;;
*) bad "positive control: suites on selects the ejected suite" "wanted [test-conf.sh] in [$sel_on]"
esac
sel_off="$(SPIRA_GATE_SUITES=off SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash "$HERE/gate-touched.sh" "$HEAD_SHA" "$HEAD_SHA" 2>/dev/null)"
is "suites off selects nothing, not even an ejected suite" "" "$sel_off"
# The repo-map gate command's shape: an empty selection exits 0 before any suite runs.
cmd_rc() { SPIRA_GATE_SUITES="$1" SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash -c '
    _s="$(bash "$1/gate-touched.sh" "$0" "$0")"; [ -n "$_s" ] || exit 0; exit 9' "$HEAD_SHA" "$HERE" 2>/dev/null; echo $?; }
is "gate command passes after the fences when suites are off" "0" "$(cmd_rc off)"
is "positive control: gate command reaches the suites when on" "9" "$(cmd_rc on)"

echo "2. gate.sh cache key:"
fn="$(awk '/^gate_key\(\) \{/{f=1} f{print} f&&/^\}/{exit}' "$HERE/gate.sh")"
if [ -z "$fn" ]; then
    bad "gate_key located (positive control)" "awk extracted nothing"
else
    ok "gate_key located (positive control)"
    # gate_key() now calls gate_key_hash() (gate-lib.sh) for the pure hashing step —
    # sourced here too, since $fn is only the gate_key() body, extracted by awk.
    key() { ( . "$HERE/gate-lib.sh"; eval "$fn"; REPO="$FIX"; BR=HEAD; files="spira/gate.sh"; CMD="bash x"
              EXCLUDE="$HERE/gate.sh"; SKEW="$HERE/gate.sh"; REPO_NAME=spira
              SPIRA_GATE_SUITES="$1" gate_key ); }
    k_on="$(key on)"; k_on2="$(key on)"; k_off="$(key off)"
    [ -n "$k_on" ] && ok "key computed" || bad "key computed" "empty"
    is "positive control: same mode, same key" "$k_on" "$k_on2"
    [ "$k_on" != "$k_off" ] && ok "suites=off and suites=on have different keys" \
        || bad "suites=off and suites=on have different keys" "both $k_on"
fi

echo "3. the switch survives gate.sh's env -i:"
# gate.sh runs the repo command under `env -i` with an allowlist. #281 shipped without
# SPIRA_GATE_SUITES on it, so the switch reached gate.sh and died there: every property above
# held and certification still ran full suites. Assert the handoff, not only the ends.
envblk="$(awk '/env -i \\$/{f=1} f{print} f&&/bash -c "\$CMD"/{exit}' "$HERE/gate.sh")"
[ -n "$envblk" ] && ok "gate.sh env -i block located (positive control)" \
    || bad "gate.sh env -i block located" "awk extracted nothing"
case "$envblk" in *'SPIRA_GATE_SUITES="${SPIRA_GATE_SUITES:-on}"'*) ok "env -i passes SPIRA_GATE_SUITES to the gate command" ;;
    *) bad "env -i passes SPIRA_GATE_SUITES to the gate command" "missing from the allowlist" ;; esac

echo "4. landing.sh call sites:"
n_cert="$(grep -c 'SPIRA_GATE_SUITES="${SPIRA_CERTIFY_SUITES:-on}"' "$HERE/landing.sh")"
is "both queue-mode certification calls pass the switch" "2" "$n_cert"
n_gate="$(grep -c '"$SPIRA_HOME/gate.sh"' "$HERE/landing.sh")"
is "positive control: landing.sh has three gate.sh calls" "3" "$n_gate"
tl_summary
