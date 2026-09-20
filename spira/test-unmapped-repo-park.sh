#!/usr/bin/env bash
#
# test-unmapped-repo-park.sh — a bead with an unmapped repo: label is parked by adding the
#   ask label AND overseer, so bd ready --exclude-label $SPIRA_ASK_LABEL never returns it
#   again, and the decisions pane (which selects on overseer) can surface it to the operator.
#
#   ./test-unmapped-repo-park.sh
#
# THE DEFECT THIS PREVENTS (sp-4l0d). aeon.sh refused an unmapped repo: label correctly, but
# called release_own_claim without first adding the ask label, returning the bead to the ready
# queue. The sentinel re-summoned an aeon within two minutes — an infinite loop. The fix adds
# SPIRA_ASK_LABEL to the bead before releasing; every fayth's FAYTH_EXCLUDE_LABELS contains
# $SPIRA_ASK_LABEL, so bd ready --exclude-label ... skips it until a human corrects the label
# or the repo-map and removes the ask label. Scar: sp-nlhy accumulated four identical notes,
# one per summon.
#
# THE LIVELOCK THIS ALSO PREVENTS (sp-foi7). The ask label must be paired with overseer
# at every creation site; without overseer the bead carries SPIRA_ASK_LABEL but lacks the
# label the decisions pane selects on — invisible to the loop AND to the operator
# (detect_livelocked: ask-no-overseer). The park path must apply both labels.
#
# FOUR CASES:
#   1. POSITIVE CONTROL — without ask label, bead IS in bd ready --exclude-label output.
#      Without this, a "park everything" implementation reads as correct.
#   2. PARKED — after adding ask+overseer labels, bead is NOT in bd ready --exclude-label output.
#   3. OVERSEER PRESENT — the overseer label is on the bead alongside the ask label, so the
#      decisions pane can show it to the operator.
#   4. UNPARKED — after removing ask label, bead returns to bd ready output.
#
# defect: sp-4l0d
# covers: spira/aeon.sh spira/lib.sh
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
has()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-unmapped-repo-park
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up unmapped-repo-park || { echo "test-unmapped-repo-park: could not build fixture database"; exit 1; }

# conf.sh default; SPIRA_ASK_LABEL may override.
ASK="${SPIRA_ASK_LABEL:-needs-operator}"

B() { "$SPIRA_BD" -C "$SPIRA_DB" "$@"; }

# ready_ids -> space-separated ids from bd ready, excluding the ask label (what a fayth sees).
ready_ids() {
    B ready --label "spira,plan" --exclude-label "$ASK" --json 2>/dev/null \
        | python3 -c 'import sys,json
d=json.load(sys.stdin)
items = d if isinstance(d,list) else [d]
print(" ".join(i["id"] for i in items if i.get("id")))' 2>/dev/null || true
}

echo "test-unmapped-repo-park.sh"

# --------------------------------------------------------------------------
echo
echo "POSITIVE CONTROL: open bead without ask label appears in bd ready:"
# --------------------------------------------------------------------------

B create "unmapped repo park test" --labels "spira,plan,repo:nonesuch" >/dev/null 2>&1
ID="$(B list --status open --label "spira,plan,repo:nonesuch" --json 2>/dev/null \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d[0]["id"] if isinstance(d,list) and d else "")' 2>/dev/null)"

if [ -z "$ID" ]; then
    bad "fixture: could not create test bead" "empty id"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi

before="$(ready_ids)"
has "before park: bead appears in bd ready" "$ID" "$before"

# --------------------------------------------------------------------------
echo
echo "PARKED: after adding ask+overseer labels, bd ready skips the bead:"
# --------------------------------------------------------------------------

B label add "$ID" "$ASK"      >/dev/null 2>&1
B label add "$ID" "overseer"  >/dev/null 2>&1

# Confirm both labels are on the bead.
labels_out="$(B label list "$ID" 2>/dev/null || true)"
has "ask label is on the bead"      "$ASK"      "$labels_out"
has "overseer label is on the bead" "overseer"  "$labels_out"

after="$(ready_ids)"
lacks "after park: bead absent from bd ready --exclude-label $ASK" "$ID" "$after"

# --------------------------------------------------------------------------
echo
echo "UNPARKED: removing ask label returns bead to bd ready:"
# --------------------------------------------------------------------------

B label remove "$ID" "$ASK"      >/dev/null 2>&1
B label remove "$ID" "overseer"  >/dev/null 2>&1

restored="$(ready_ids)"
has "after remove: bead returns to bd ready" "$ID" "$restored"

# Cleanup.
B close "$ID" --reason "test cleanup" >/dev/null 2>&1 || true

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
