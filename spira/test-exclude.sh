#!/usr/bin/env bash
#
# test-exclude.sh — exclude.sh keeps beads data out of the harness tree and out of
# staged files (UC-safety-fences-23).
#
# WHY THIS EXISTS
# ----------------
# exclude.sh is fence one of gate-spira.sh's own list (line 111) and fence one of
# hooks/pre-commit, and law-beads-is-never-public rests on it, but no suite drove it
# before this one (gap 4 in docs/test-plan/safety-fences.md).
#
# FAIL-CLOSED ROWS FIRST. `filter` and `scope` both refuse an input that carries no
# harness signature (boundary + gate.sh + lib.sh together) rather than silently
# filtering nothing — that refusal is asserted before any "clean input passes" row, so
# a `filter` that always exits 0 cannot pass this suite by accident
# (law-absence-needs-a-positive-control).
#
# tier: T2
# covers: spira/exclude.sh UC-safety-fences-23
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-exclude.sh"

EXCLUDE="$HERE/exclude.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A path list carrying the harness signature (boundary + gate.sh + lib.sh in one
# directory), so `filter`/`scope` can resolve a scope with no filesystem access.
SIGNATURE=$'spira/boundary\nspira/gate.sh\nspira/lib.sh\n'

filter_with() { printf '%s' "$1" | bash "$EXCLUDE" filter; }

# ===========================================================================
echo
echo "FAIL-CLOSED — no harness signature in the path list:"
# ===========================================================================
out="$(printf 'a/b.txt\nc/d.jsonl\n' | bash "$EXCLUDE" filter 2>&1)"; rc=$?
is   "no signature: filter exits 3, refusing to guard nothing" "3" "$rc"
want "no signature: says why"                                  "nothing to guard" "$out"

# ===========================================================================
echo
echo "POSITIVE CONTROL — filter names every forbidden shape once a scope resolves:"
# ===========================================================================
list="${SIGNATURE}.beads/config.yaml
.dolt/noms/manifest
embeddeddolt/x
proxieddb/y
export.jsonl
state.db
state.db-wal
state.sqlite3
.beads-credential-key
sub/dir/.beads/metadata.json"
out="$(filter_with "$list")"; rc=$?
is   "mixed list: filter exits 0 (found offenders)" "0" "$rc"
for p in .beads/config.yaml .dolt/noms/manifest embeddeddolt/x proxieddb/y \
         export.jsonl state.db state.db-wal state.sqlite3 .beads-credential-key \
         sub/dir/.beads/metadata.json; do
    want "POSITIVE: flags $p" "$p" "$out"
done

# ===========================================================================
echo
echo "clean paths are never flagged, once the positive control above is trusted:"
# ===========================================================================
clean="${SIGNATURE}spira/aeon.sh
docs/readme.md
sub/dir/notes.md
a.jsonline.txt"
out="$(filter_with "$clean")"; rc=$?
is     "clean list: filter exits 1 (nothing forbidden)" "1" "$rc"
is     "clean list: prints nothing"                     "" "$out"

# ===========================================================================
echo
echo "scope is derived from the signature, not listed: a harness one level under the"
echo "root widens the scope to '.'; buried deeper, only its own subtree is in scope:"
# ===========================================================================
out="$(printf '%s' "${SIGNATURE}.beads/x" | bash "$EXCLUDE" scope 2>&1)"
is "harness at root: scope is '.'" "." "$out"

nested=$'wiki/.claude/spira/boundary\nwiki/.claude/spira/gate.sh\nwiki/.claude/spira/lib.sh\n'
out="$(printf '%s' "$nested" | bash "$EXCLUDE" scope 2>&1)"
is "harness buried deeper: scope stays that directory" "wiki/.claude/spira" "$out"

# A .beads/ tree OUTSIDE the resolved scope is another repository's business, not this
# fence's — in_scope must actually narrow what filter reports.
out="$(printf '%s' "${nested}outside/.beads/config.yaml" | bash "$EXCLUDE" filter 2>&1)"; rc=$?
is     "out-of-scope .beads/ is not flagged" "1" "$rc"
nowant "out-of-scope .beads/ is not flagged" "outside/.beads" "$out"

# ===========================================================================
echo
echo "T2 — staged: the pre-commit entry point, against a real scratch repository:"
# ===========================================================================
REPO="$TMP/repo"
mkdir -p "$REPO/spira"
: > "$REPO/spira/boundary"
: > "$REPO/spira/gate.sh"
: > "$REPO/spira/lib.sh"
git init -q -b main "$REPO"
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t
git -C "$REPO" add -A
git -C "$REPO" commit -q -m init

# POSITIVE CONTROL: a staged offender is refused before a clean stage is trusted.
mkdir -p "$REPO/.beads"
echo x > "$REPO/.beads/config.yaml"
git -C "$REPO" add -A
out="$(bash "$EXCLUDE" staged "$REPO" 2>&1)"; rc=$?
is   "POSITIVE: staged .beads/config.yaml → staged refuses" "1" "$rc"
want "POSITIVE: refusal names the offending path"           ".beads/config.yaml" "$out"
want "POSITIVE: refusal names the override"                 "git restore --staged" "$out"
git -C "$REPO" reset -q --hard
git -C "$REPO" clean -qfdx

# SEEN GREEN: a clean stage passes.
echo more >> "$REPO/spira/lib.sh"
git -C "$REPO" add -A
out="$(bash "$EXCLUDE" staged "$REPO" 2>&1)"; rc=$?
is "clean stage: staged exits 0" "0" "$rc"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
