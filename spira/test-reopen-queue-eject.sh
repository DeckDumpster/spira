#!/usr/bin/env bash
# test-reopen-queue-eject.sh — re-certification after ejection runs the ejecting suite.
#
# covers: spira/gate-touched.sh spira/verdict.sh spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "want [$2] got [$3]"; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---------------------------------------------------------------------------
# FIXTURE: a branch that changes spira/batch.sh.
# test-batch.sh covers spira/batch.sh  — selected when batch.sh changes.
# test-other.sh covers spira/other.sh  — NOT selected unless ejected.
# Both suites exist in the fixture repo so the ejected-suite file check passes.
# ---------------------------------------------------------------------------
R="$TMP/repo"; git init -q -b main "$R"; mkdir -p "$R/spira"
printf '#!/usr/bin/env bash\n# covers: spira/batch.sh\nexit 0\n' > "$R/spira/test-batch.sh"
printf '#!/usr/bin/env bash\n# covers: spira/other.sh\nexit 0\n' > "$R/spira/test-other.sh"
printf 'x\n' > "$R/spira/batch.sh"
printf 'x\n' > "$R/spira/other.sh"
git -C "$R" add -A; git -C "$R" commit -q -m base
git -C "$R" checkout -q -b spira/sp-x
printf 'y\n' >> "$R/spira/batch.sh"
git -C "$R" add -A; git -C "$R" commit -q -m "fix batch"

touched() {
    (cd "$R" && SPIRA_GATE_EJECTED_SUITES="${1:-}" SPIRA_GATE_REPO="$R" \
        SPIRA_BATCH_SUITE_DIR="$R/spira" \
        bash "$HERE/gate-touched.sh" main spira/sp-x 2>/dev/null \
        | sort | tr '\n' ' ' | sed 's/ $//')
}

echo "test-reopen-queue-eject.sh"
echo

# ---------------------------------------------------------------------------
# POSITIVE CONTROL: without ejection context, only the covering suite is
# selected. test-other.sh covers a file not in the diff — it must not appear.
# This proves the mechanism: the ejection path is the ONLY thing that adds it.
# ---------------------------------------------------------------------------
echo "positive control — branch touching only batch.sh, no ejection context:"
is "covering suite selected, ejected suite absent" "test-batch.sh" "$(touched '')"

# ---------------------------------------------------------------------------
# FIXED BEHAVIOUR: with SPIRA_GATE_EJECTED_SUITES naming test-other.sh,
# gate-touched.sh includes it regardless of the branch diff.
# ---------------------------------------------------------------------------
echo
echo "fixed — ejected suite added to selection:"
is "SPIRA_GATE_EJECTED_SUITES: ejected suite included" \
    "test-batch.sh test-other.sh" "$(touched 'test-other.sh')"

# ---------------------------------------------------------------------------
# MULTIPLE EJECTED SUITES (CSV).
# ---------------------------------------------------------------------------
is "comma-separated ejected suites: both included" \
    "test-batch.sh test-other.sh" "$(touched 'test-batch.sh,test-other.sh')"

# ---------------------------------------------------------------------------
# DEDUP: if the ejected suite is also covered by the diff, it appears once.
# test-batch.sh is both selected by coverage AND listed as ejected.
# ---------------------------------------------------------------------------
is "dedup: ejected suite covered by diff appears only once" \
    "test-batch.sh" "$(touched 'test-batch.sh')"

# ---------------------------------------------------------------------------
# ABSENT SUITE: an ejected suite that no longer exists in the tree is skipped.
# (Use a fresh branch that touches no file — absent suite must not appear.)
# ---------------------------------------------------------------------------
git -C "$R" checkout -q -b spira/sp-y main 2>/dev/null
printf 'z\n' >> "$R/spira/batch.sh"
git -C "$R" add -A; git -C "$R" commit -q -m "fix batch y"
absent() {
    (cd "$R" && SPIRA_GATE_EJECTED_SUITES="${1:-}" SPIRA_GATE_REPO="$R" \
        SPIRA_BATCH_SUITE_DIR="$R/spira" \
        bash "$HERE/gate-touched.sh" main spira/sp-y 2>/dev/null \
        | sort | tr '\n' ' ' | sed 's/ $//')
}
is "absent ejected suite is silently skipped" \
    "test-batch.sh" "$(absent 'test-gone.sh')"

# ---------------------------------------------------------------------------
# LANDSTATE FORMAT: _attr_eject stores the suites CSV in the EJECTED record
# so the gate can read them. Verified by calling land_mark directly and reading
# back the reason field.
# ---------------------------------------------------------------------------
echo
echo "landstate — ejected record carries suite list:"
. "$HERE/lib.sh" 2>/dev/null
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN/landstate"
LANDSTATE="$SPIRA_RUN/landstate"
land_mark sp-x EJECTED abc123 "test-batch.sh,test-other.sh"
_st="" _tip="" _epoch="" _csv=""
{ read -r _st _tip _epoch _csv < "$LANDSTATE/sp-x"; } 2>/dev/null || true
is "state is EJECTED"                     "EJECTED"                       "$_st"
is "tip recorded"                         "abc123"                        "$_tip"
is "suites CSV in reason field"           "test-batch.sh,test-other.sh"   "$_csv"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
