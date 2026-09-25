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
# WIDENING PARITY (sp-aoads). `filter` widens a harness one level under the root to "."
# with the same rule `scope`/`check`/`staged`/`install` use, so a `.beads/` at the
# repository root is caught by `git ls-files | exclude.sh filter` — gate-spira.sh's own
# landing-gate call — exactly when `check` and the pre-commit `staged` hook would also
# catch it. The "widened" row below asserts that parity against an identical tree.
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
# "spira" sits one level under the root, so `filter`'s scope WIDENS to "." (see the
# parity note above) — every row below is in scope no matter what prefix it carries.
SIGNATURE=$'spira/boundary\nspira/gate.sh\nspira/lib.sh\n'

# A signature nested TWO levels under the root ("vendor/spira/") does NOT widen, so it
# is used below to test in_scope narrowing on its own — widening would otherwise make
# every path in scope and the narrowing assertion vacuous.
NESTED_SIGNATURE=$'vendor/spira/boundary\nvendor/spira/gate.sh\nvendor/spira/lib.sh\n'

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
echo "in_scope narrows what filter reports: a path outside a NESTED (non-widening)"
echo "harness directory is not this fence's business, whether or not it looks forbidden:"
# ===========================================================================
out="$(filter_with "${NESTED_SIGNATURE}outside/.beads/config.yaml")"; rc=$?
is     "out-of-scope .beads/ is not flagged" "1" "$rc"
nowant "out-of-scope .beads/ is not flagged" "outside/.beads" "$out"
out="$(filter_with "${NESTED_SIGNATURE}vendor/spira/.beads/config.yaml")"; rc=$?
is   "in-scope (under nested harness dir) .beads/ is flagged" "0" "$rc"
want "in-scope (under nested harness dir) .beads/ is flagged" "vendor/spira/.beads/config.yaml" "$out"

# ===========================================================================
echo
echo "WIDENING PARITY (sp-aoads) — filter widens scope the same way check/staged do:"
echo "a repo-root offender outside a one-level-under-root harness dir is caught by"
echo "filter, exactly as check and staged both catch it on the identical tree:"
# ===========================================================================
WIDEROOT="$TMP/wideroot"
mkdir -p "$WIDEROOT/spira" "$WIDEROOT/.beads"
: > "$WIDEROOT/spira/boundary"; : > "$WIDEROOT/spira/gate.sh"; : > "$WIDEROOT/spira/lib.sh"
echo secret > "$WIDEROOT/.beads/config.yaml"
git init -q -b main "$WIDEROOT"
git -C "$WIDEROOT" config user.email t@t
git -C "$WIDEROOT" config user.name t
git -C "$WIDEROOT" add -A
git -C "$WIDEROOT" commit -q -m init

out="$(git -C "$WIDEROOT" ls-files | bash "$EXCLUDE" filter 2>&1)"; rc=$?
is   "sp-aoads: filter on a root-level .beads/ (widened) reports it" "0" "$rc"
want "sp-aoads: filter names the offending path"                     ".beads/config.yaml" "$out"

crc=0; bash "$EXCLUDE" check "$WIDEROOT" >/dev/null 2>&1 || crc=$?
is "check on the identical tree also refuses (same scope as filter now)" "1" "$crc"

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
# uses the widened scope, same as filter now) is refused before a clean stage is trusted.
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
