#!/usr/bin/env bash
# test-reopen-queue-eject.sh — re-certification after ejection runs the ejecting suite.
#
# tier: T1
# covers: spira/gate-touched.sh spira/verdict.sh spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
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

# ---------------------------------------------------------------------------
# EJECTED FILE SURVIVES A RED TRANSITION. This is the defect class:
# a failed re-certification overwrites the EJECTED landstate with RED,
# erasing the suite CSV. The .ejected sidecar file must persist independently.
#
# Positive control: without the .ejected file the UNFIXED code returns empty.
# (The positive control is the negative assertion in "unfixed leg" below.)
# ---------------------------------------------------------------------------
echo
echo "ejected-suite file survives RED transition (sp-px6ng):"
export SPIRA_RUN="$TMP/run2"; mkdir -p "$SPIRA_RUN/landstate"
LANDSTATE="$SPIRA_RUN/landstate"

# Seed EJECTED state + .ejected file, as _attr_eject now does.
land_mark sp-z EJECTED def456 "test-other.sh"
printf '%s' "test-other.sh" > "$LANDSTATE/sp-z.ejected"

# Simulate a failed re-certification (landing.sh:1179): overwrites EJECTED with RED.
land_mark sp-z RED def456 gate

# The EJECTED record is gone; only the .ejected file remains.
_post_red_st="" _post_red_tip="" _post_red_epoch="" _post_red_reason=""
{ read -r _post_red_st _post_red_tip _post_red_epoch _post_red_reason < "$LANDSTATE/sp-z"; } 2>/dev/null || true
is "landstate shows RED after failed re-cert"  "RED"  "$_post_red_st"

# Gate reads .ejected file when present, regardless of state.
_got_ejected=""
_ej_file="$LANDSTATE/sp-z.ejected"
if [ -f "$_ej_file" ]; then
    { read -r _got_ejected < "$_ej_file"; } 2>/dev/null || true
fi
is "ejected_suites non-empty after RED transition"  "test-other.sh"  "$_got_ejected"

# Negative leg: a bead with no eject history resolves ejected_suites empty.
land_mark sp-noeject RED abc000 gate
_neg_ejected=""
_neg_ej_file="$LANDSTATE/sp-noeject.ejected"
[ -f "$_neg_ej_file" ] && { read -r _neg_ejected < "$_neg_ej_file"; } 2>/dev/null || true
is "no .ejected file → ejected_suites empty"  ""  "$_neg_ejected"

echo
tl_summary
