#!/usr/bin/env bash
#
# test-testlib-fence.sh — testlib-fence.sh's own positive control.
#
# THE CASE. A suite rolling its own assertion helpers must be refused on the author's own
# path (queue.sh submit), statically, scoped to added/modified suites.
#
# PROPERTIES
#   T1: a branch adding a suite with a self-rolled pass() and no testlib.sh source is
#       SEEN RED, naming the file and the helper (the bead's acceptance case, exactly).
#   T1: a branch adding a suite that sources testlib.sh certifies.
#   T1: an unrelated diff (no spira/test-*.sh touched) is skipped, not judged.
#   T1: a suite on testlib_migration_exceptions is never flagged, even carrying the
#       self-rolled family the fence exists to catch.
#   T1: the git-diff fallback (SPIRA_GATE_BASE, no SPIRA_GATE_FILES) is exercised too — the
#       shape gate.sh always supplies SPIRA_GATE_FILES for, but a direct call may not.
#   T1: no diff context at all is a SKIP, not a forced refusal (build-fence.sh's own
#       convention, reused here).
#
# tier: T1
# covers: spira/testlib-fence.sh spira/testlib-migration-lib.sh spira-lint/testlib-migrated-allow spira/repo-map.example
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-testlib-fence.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# fixture <name> -> a scratch tree at $TMP/<name> carrying testlib-fence.sh and its lib,
# so the fence under test always runs from a copy rather than the checkout's own.
fixture() {
    local d="$TMP/$1"; mkdir -p "$d/spira"
    cp "$HERE/testlib-fence.sh" "$d/spira/testlib-fence.sh"
    cp "$HERE/testlib-migration-lib.sh" "$d/spira/testlib-migration-lib.sh"
    printf '%s' "$d"
}

fence_files() {   # fence_files <root> <files-list-file>
    ( cd "$1" && SPIRA_GATE_FILES="$2" bash spira/testlib-fence.sh 2>&1 )
}

echo "1. SEEN RED: a new suite defining pass() locally is refused, naming file and helper:"
F1="$(fixture red)"
cat > "$F1/spira/test-foo.sh" <<'EOF'
#!/usr/bin/env bash
pass=0; fail=0
pass() { pass=$((pass+1)); }
EOF
L1="$TMP/l1"; printf 'A\tspira/test-foo.sh\n' > "$L1"
out="$(fence_files "$F1" "$L1")"; rc=$?
is   "SEEN RED: exits 1" "1" "$rc"
want "and names the file" "spira/test-foo.sh" "$out"
want "and names the helper it found" "pass()" "$out"

echo "2. GREEN: a new suite sourcing testlib.sh certifies:"
F2="$(fixture green)"
cat > "$F2/spira/test-foo.sh" <<'EOF'
#!/usr/bin/env bash
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
ok "case one"
tl_summary
EOF
L2="$TMP/l2"; printf 'A\tspira/test-foo.sh\n' > "$L2"
out="$(fence_files "$F2" "$L2")"; rc=$?
is "GREEN: sourcing testlib.sh exits 0" "0" "$rc"
want "and says so" "no added or modified suite rolls its own" "$out"

echo "3. an unrelated diff is skipped, not judged:"
F3="$(fixture unrelated)"
L3="$TMP/l3"; printf 'M\tREADME.md\n' > "$L3"
out="$(fence_files "$F3" "$L3")"; rc=$?
is   "unrelated diff: exits 0" "0" "$rc"
want "and says why" "skipped" "$out"

echo "4. the exception list is honoured even for the self-rolled family:"
F4="$(fixture exempt)"
cat > "$F4/spira/test-install-refusal.sh" <<'EOF'
#!/usr/bin/env bash
pass=0; fail=0
ok() { pass=$((pass+1)); }
bad() { fail=$((fail+1)); }
EOF
L4="$TMP/l4"; printf 'M\tspira/test-install-refusal.sh\n' > "$L4"
out="$(fence_files "$F4" "$L4")"; rc=$?
is "exempted suite: exits 0 despite ok()/bad()" "0" "$rc"

echo "5. a modified suite already carrying the offense is caught too (not add-only):"
F5="$(fixture modified)"
cat > "$F5/spira/test-bar.sh" <<'EOF'
#!/usr/bin/env bash
is() { :; }
EOF
L5="$TMP/l5"; printf 'M\tspira/test-bar.sh\n' > "$L5"
out="$(fence_files "$F5" "$L5")"; rc=$?
is   "modified suite: exits 1" "1" "$rc"
want "and names it" "spira/test-bar.sh" "$out"

echo "6. deleted/renamed-away entries carry no new content and are not checked:"
F6="$(fixture deleted)"
# No spira/test-baz.sh on disk at all — a deleted file. The status line names it, but
# [ -f "$f" ] must skip it rather than erroring on a path that no longer exists.
L6="$TMP/l6"; printf 'D\tspira/test-baz.sh\n' > "$L6"
out="$(fence_files "$F6" "$L6")"; rc=$?
is "a deleted suite is not checked" "0" "$rc"

echo "7. STATUS<TAB>FILE form (gate.sh's own shape) and bare FILE form both select:"
F7="$(fixture bare)"
cp "$F1/spira/test-foo.sh" "$F7/spira/test-foo.sh"
L7="$TMP/l7"; printf 'spira/test-foo.sh\n' > "$L7"
out="$(fence_files "$F7" "$L7")"; rc=$?
is "bare FILE form still catches the offender" "1" "$rc"

echo "8. the git-diff fallback (SPIRA_GATE_BASE, no SPIRA_GATE_FILES):"
F8="$(fixture gitbase)"
git -C "$F8" init -q -b main
git -C "$F8" add -A
git -C "$F8" -c user.name=t -c user.email=t@t commit -q -m base
BASE_SHA="$(git -C "$F8" rev-parse HEAD)"
cat > "$F8/spira/test-foo.sh" <<'EOF'
#!/usr/bin/env bash
pass=0; fail=0
pass() { pass=$((pass+1)); }
EOF
git -C "$F8" add -A
git -C "$F8" -c user.name=t -c user.email=t@t commit -q -m "add offending suite"
out="$( (cd "$F8" && SPIRA_GATE_BASE="$BASE_SHA" bash spira/testlib-fence.sh) 2>&1 )"; rc=$?
is   "git-diff fallback: SEEN RED" "1" "$rc"
want "and names the file" "spira/test-foo.sh" "$out"

echo "8b. GREEN AFTER: the same suite, migrated to testlib.sh, passes (positive control):"
cat > "$F8/spira/test-foo.sh" <<'EOF'
#!/usr/bin/env bash
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_summary
EOF
git -C "$F8" add -A
git -C "$F8" -c user.name=t -c user.email=t@t commit -q -m "migrate to testlib.sh"
out="$( (cd "$F8" && SPIRA_GATE_BASE="$BASE_SHA" bash spira/testlib-fence.sh) 2>&1 )"; rc=$?
is "GREEN AFTER: migrated suite certifies" "0" "$rc"

echo "9. no diff context at all is a SKIP, not a forced refusal:"
F9="$(fixture nocontext)"
out="$( (cd "$F9" && bash spira/testlib-fence.sh) 2>&1 )"; rc=$?
is   "no SPIRA_GATE_FILES/SPIRA_GATE_BASE: exits 0" "0" "$rc"
want "and says it has no diff to check" "no diff to check" "$out"

# =========================================================================================
# GATE INTEGRATION. A fence nothing invokes is a file.
# =========================================================================================
echo "10. gate integration:"
rm_src="$(cat "$HERE/repo-map.example")"
want "repo-map.example's harness row calls testlib-fence.sh" "testlib-fence.sh" "$rm_src"
case "$rm_src" in
    *'harness'*'queue'*'testlib-fence.sh'*) ok "testlib-fence.sh is in the queue-mode harness row's gate command" ;;
    *) bad "testlib-fence.sh is in the queue-mode harness row's gate command" "not found on that row" ;;
esac
is "testlib-fence.sh is executable" "0" "$([ -x "$HERE/testlib-fence.sh" ]; echo $?)"

tl_summary
