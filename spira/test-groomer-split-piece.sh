#!/usr/bin/env bash
#
# test-groomer-split-piece.sh — groomer.sh split-piece gives every piece of a split its own
#                                branch; a bare `bd create --parent` does not.
#
#   ./test-groomer-split-piece.sh
#
# THE DEFECT THIS REPRODUCES. `bd create --parent` inherits every label from the parent by
# default, and a bead's branch affinity IS a label (branch:<name> — bead_branch, lib.sh).
# sp-zs04v was split into four children this way and three of them (sp-zs04v.2/.5/.6)
# inherited branch:spira/sp-zs04v — the PARENT's branch — so all three resolved to the same
# branch as their parent and, by extension, as each other (law-one-aeon-one-worktree).
#
# THE SAME INHERITANCE HANDS delivers: TO A CHILD TOO. A child that inherits a parent's
# delivers:beads or delivers:note:/path label claims delivery of evidence it never produced
# (aeon.sh, sentinel.sh CHECK 5 verify against it) — measured against 18 real children that
# inherited a parent's branch: the same way (sp-om71s).
#
# THE PLANTED OFFENDER (law-a-check-that-finds-nothing-must-first-prove-it-could-have-found-
# something): a bare `bd create --parent` against the real database, with no other change,
# must be seen to reproduce the inherited branch before the fix (split-piece) is trusted to
# avoid it — otherwise this suite could pass on a database that never exhibited the defect.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): the assertion on split-piece's
# child is that its branch EQUALS spira/<child-id>, not merely that it differs from the
# parent's — a bug that produced some OTHER wrong branch would defeat a negative-only check.
#
# Driven against a real bd (testdb.sh) rather than the argv-recording stub test-groomer.sh
# uses elsewhere in this file's siblings, because the property under test IS bd's own label-
# inheritance behaviour on --parent — a stub that only records arguments cannot exhibit it
# (law-prefer-the-real-dependency).
#
# defect: sp-gseub
# covers: spira/groomer.sh spira/lib.sh spira/chamber/groomer.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
labels_of() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    print(" ".join(d[0].get("labels") or []))
except Exception: pass
' 2>/dev/null; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-groomer-split-piece
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up groomersplitpiece || { echo "test-groomer-split-piece: could not build a fixture database"; exit 1; }
testdb_reset

GROOMSH="$HERE/groomer.sh"
export SPIRA_CONF="/nonexistent-$$.conf"

seed() {
    printf '{"id":"%s","title":"%s","status":"open","issue_type":"task","labels":["plan","repo:fixture"],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$2" | testdb_seed
}

echo
echo "test-groomer-split-piece.sh"
echo

# ======================================================================================
echo "PLANT THE OFFENDER: a bare 'bd create --parent' inherits the parent's branch:"
# ======================================================================================
seed sp-tgsp-bare "parent, split by hand"
bd -C "$SPIRA_DB" set-state sp-tgsp-bare "branch=spira/sp-tgsp-bare" >/dev/null 2>&1
bd -C "$SPIRA_DB" label add sp-tgsp-bare "delivers:beads" >/dev/null 2>&1

bare_child="$(bd -C "$SPIRA_DB" create --parent sp-tgsp-bare --title "bare piece" --type task \
    -l "plan,repo:fixture" --silent 2>/dev/null)"
[ -n "$bare_child" ] || { echo "test-groomer-split-piece: setup failed: bd create --parent returned no id" >&2; exit 1; }
bare_branch="$(bd -C "$SPIRA_DB" state "$bare_child" branch 2>/dev/null)"
is "offender: bare create --parent hands the child the PARENT's branch" \
   "spira/sp-tgsp-bare" "$bare_branch"
case " $(labels_of "$bare_child") " in
    *" delivers:beads "*) ok "offender: bare create --parent hands the child the PARENT's delivers: too" ;;
    *) bad "offender: bare create --parent hands the child the PARENT's delivers: too" "not found in: $(labels_of "$bare_child")" ;;
esac

echo
# ======================================================================================
echo "split-piece gives the new piece its OWN branch, derived from its own id:"
# ======================================================================================
seed sp-tgsp-orig "parent, split via split-piece"
bd -C "$SPIRA_DB" set-state sp-tgsp-orig "branch=spira/sp-tgsp-orig" >/dev/null 2>&1
bd -C "$SPIRA_DB" label add sp-tgsp-orig "delivers:beads" >/dev/null 2>&1

child="$("$GROOMSH" split-piece sp-tgsp-orig --title "piece one" --type task -l "plan,repo:fixture" 2>"$TMP/err")"
rc=$?
is "split-piece exits 0" "0" "$rc"
[ -n "$child" ] || { echo "test-groomer-split-piece: split-piece returned no id" >&2; exit 1; }

child_branch="$(bd -C "$SPIRA_DB" state "$child" branch 2>/dev/null)"
is "split-piece: child's branch is its OWN, spira/<child-id> (positive control)" \
   "spira/$child" "$child_branch"
[ "$child_branch" != "spira/sp-tgsp-orig" ] \
    && ok "split-piece: child's branch is not the parent's" \
    || bad "split-piece: child's branch is not the parent's" "got the parent's branch: $child_branch"

case " $(labels_of "$child") " in
    *" delivers:"*) bad "split-piece: child does not inherit the parent's delivers:" "got: $(labels_of "$child")" ;;
    *) ok "split-piece: child does not inherit the parent's delivers:" ;;
esac

# The child must actually be a child of the original (--parent honoured).
parent_of_child="$(bd -C "$SPIRA_DB" children sp-tgsp-orig 2>/dev/null)"
case "$parent_of_child" in
    *"$child"*) ok "split-piece: the new piece is recorded as a child of the original" ;;
    *) bad "split-piece: the new piece is recorded as a child of the original" "not found in: $parent_of_child" ;;
esac

echo
# ======================================================================================
echo "split-piece: usage errors"
# ======================================================================================
"$GROOMSH" split-piece >/dev/null 2>&1
is "split-piece with no id exits 1" "1" "$?"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
