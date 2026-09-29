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
#   2, 3. the cache key and the env -i handoff: unit tests of the Rust gate (see below).
#
# tier: T1
# covers: spira/gate-touched.sh spira/conf.sh
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

# SPIRA_GATE_REPO/SPIRA_BATCH_SUITE_DIR point every call below at the fixture built
# above instead of this repo's own 500+ suites: gate-touched.sh's build-fence,
# tier-budget check-areas and plan-matrix-fence all used to run unconditionally on a call (all
# spira-lint rules now, sp-ufbkh)
# whose SPIRA_GATE_REPO resolves to the real tree (see gate-touched.sh's own
# comment on that gate), which used to make every call here pay a real cargo
# build and a full-corpus scan for a switch this suite never needs to exercise
# against the real tree at all.
export SPIRA_GATE_REPO="$FIX" SPIRA_BATCH_SUITE_DIR="$FIX/spira"

echo "1. gate-touched.sh:"
sel_on="$(SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash "$HERE/gate-touched.sh" "$HEAD_SHA" "$HEAD_SHA" 2>/dev/null)"
is "positive control: suites on selects the ejected suite" "test-conf.sh" "$sel_on"
sel_off="$(SPIRA_GATE_SUITES=off SPIRA_GATE_EJECTED_SUITES=test-conf.sh bash "$HERE/gate-touched.sh" "$HEAD_SHA" "$HEAD_SHA" 2>/dev/null)"
is "suites off selects nothing, not even an ejected suite" "" "$sel_off"
# The repo-map gate command's shape: an empty selection exits 0 before any suite runs.
cmd_rc() { SPIRA_GATE_SUITES="$1" SPIRA_GATE_EJECTED_SUITES=test-conf.sh \
    SPIRA_GATE_REPO="$FIX" SPIRA_BATCH_SUITE_DIR="$FIX/spira" bash -c '
    _s="$(bash "$1/gate-touched.sh" "$0" "$0")"; [ -n "$_s" ] || exit 0; exit 9' "$HEAD_SHA" "$HERE" 2>/dev/null; echo $?; }
is "gate command passes after the fences when suites are off" "0" "$(cmd_rc off)"
is "positive control: gate command reaches the suites when on" "9" "$(cmd_rc on)"

# 2 AND 3 MOVED WITH THE GATE (sp-0tpcs). The cache key and the gate command's env -i
# allowlist are the Rust gate's now, and are unit-tested there (`cargo test -p gate`):
# key.rs every_input_moves_the_key (suites=on/off move the key; the same inputs twice give the
# same key) and tests.rs fences_only_certification_takes_no_admission_slot (SPIRA_GATE_SUITES
# reaches the gate command's environment).

tl_summary
