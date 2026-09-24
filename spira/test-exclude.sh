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
# FAIL-CLOSED ROWS FIRST. `filter` refuses an input that carries no harness signature
# (boundary + gate.sh + lib.sh together) rather than silently filtering nothing — that
# refusal is asserted before any "clean input passes" row, so a `filter` that always
# exits 0 cannot pass this suite by accident (law-absence-needs-a-positive-control).
#
# KNOWN GAP, DOCUMENTED RATHER THAN FIXED (sp-aoads). `filter` computes its scope with
# the raw, unwidened `scope_from_paths`, while `scope`/`check`/`staged`/`install` all
# widen a harness one level under the root to ".". In the harness's current layout
# (boundary/gate.sh/lib.sh under `spira/`, one level under the repo root) that means
# `git ls-files | exclude.sh filter` — gate-spira.sh's own landing-gate call — never
# scans the repository ROOT, while `check` and the pre-commit `staged` hook both do.
# The "no-widen" row below asserts the CURRENT (buggy) behaviour; flip it once sp-aoads
# lands.
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
# directory, "spira/") so `filter` can resolve a scope with no filesystem access.
# `filter`'s scope is NOT widened (see the gap note above), so it stays "spira" —
# every in-scope row below is written under that prefix on purpose.
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
list="${SIGNATURE}spira/.beads/config.yaml
spira/.dolt/noms/manifest
spira/embeddeddolt/x
spira/proxieddb/y
spira/export.jsonl
spira/state.db
spira/state.db-wal
spira/state.sqlite3
spira/.beads-credential-key
spira/sub/dir/.beads/metadata.json"
out="$(filter_with "$list")"; rc=$?
is   "mixed list: filter exits 0 (found offenders)" "0" "$rc"
for p in spira/.beads/config.yaml spira/.dolt/noms/manifest spira/embeddeddolt/x \
         spira/proxieddb/y spira/export.jsonl spira/state.db spira/state.db-wal \
         spira/state.sqlite3 spira/.beads-credential-key spira/sub/dir/.beads/metadata.json; do
    want "POSITIVE: flags $p" "$p" "$out"
done

# ===========================================================================
echo
echo "clean paths are never flagged, once the positive control above is trusted:"
# ===========================================================================
clean="${SIGNATURE}spira/aeon.sh
spira/docs/readme.md
spira/sub/dir/notes.md
spira/a.jsonline.txt"
out="$(filter_with "$clean")"; rc=$?
is     "clean list: filter exits 1 (nothing forbidden)" "1" "$rc"
is     "clean list: prints nothing"                     "" "$out"

# ===========================================================================
echo
echo "in_scope narrows what filter reports: a path outside the signature's directory"
echo "is not this fence's business, whether or not it looks forbidden:"
# ===========================================================================
out="$(filter_with "${SIGNATURE}outside/.beads/config.yaml")"; rc=$?
is     "out-of-scope .beads/ is not flagged" "1" "$rc"
nowant "out-of-scope .beads/ is not flagged" "outside/.beads" "$out"

# ===========================================================================
echo
echo "KNOWN GAP (sp-aoads) — filter does not widen scope the way check/staged do:"
echo "a repo-root offender outside the one-level-under-root harness dir is missed by"
echo "filter today, even though check and staged both catch the identical tree:"
# ===========================================================================
GAPROOT="$TMP/gaproot"
mkdir -p "$GAPROOT/spira" "$GAPROOT/.beads"
: > "$GAPROOT/spira/boundary"; : > "$GAPROOT/spira/gate.sh"; : > "$GAPROOT/spira/lib.sh"
echo secret > "$GAPROOT/.beads/config.yaml"
git init -q -b main "$GAPROOT"
git -C "$GAPROOT" config user.email t@t
git -C "$GAPROOT" config user.name t
git -C "$GAPROOT" add -A
git -C "$GAPROOT" commit -q -m init

out="$(git -C "$GAPROOT" ls-files | bash "$EXCLUDE" filter 2>&1)"; rc=$?
is     "sp-aoads: filter on a root-level .beads/ (current, buggy) reports clean" "1" "$rc"
is     "sp-aoads: filter prints nothing"                                        "" "$out"

crc=0; bash "$EXCLUDE" check "$GAPROOT" >/dev/null 2>&1 || crc=$?
is "check on the identical tree still refuses (scope is widened there)" "1" "$crc"

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

# POSITIVE CONTROL: a staged offender (even at the repo root, outside spira/ — staged
# uses the WIDENED scope, unlike filter) is refused before a clean stage is trusted.
mkdir -p "$REPO/.beads"
echo x > "$REPO/.beads/config.yaml"
git -C "$REPO" add -A
out="$(bash "$EXCLUDE" staged "$REPO" 2>&1)"; rc=$?
is   "POSITIVE: staged root-level .beads/config.yaml → staged refuses" "1" "$rc"
want "POSITIVE: refusal names the offending path"                      ".beads/config.yaml" "$out"
want "POSITIVE: refusal names the override"                            "git restore --staged" "$out"
git -C "$REPO" reset -q --hard
git -C "$REPO" clean -qfdx

# SEEN GREEN: a clean stage passes.
echo more >> "$REPO/spira/lib.sh"
git -C "$REPO" add -A
out="$(bash "$EXCLUDE" staged "$REPO" 2>&1)"; rc=$?
is "clean stage: staged exits 0" "0" "$rc"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
