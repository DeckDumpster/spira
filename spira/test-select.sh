#!/usr/bin/env bash
# test-select.sh — select.sh: the one suite selector
#
# WHAT THIS PROVES
#   1. --all outputs every suite in SUITE_DIR; mode-file gets "all"
#   2. diff mode with a covered change selects the covering suite + always-run suites
#      mode-file gets "diff"
#   3. diff mode with an unmapped change falls back to all suites; mode-file gets "all"
#   4. diff mode with no changed files selects only no-covers (always-run) suites
#      mode-file gets "diff"
#   5. gate-spira.sh and testenv-batch.sh contain no inline selection loop;
#      both delegate to select.sh
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
#   • B: suite A is the planted offender that must appear to trust the selection
#   • C: all three suites are the planted offenders for unmapped fallback
#
# covers: spira/select.sh spira/gate-spira.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iseq()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$3], got [$2]"; }

SELECT="$HERE/select.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-select.sh"

# ---------------------------------------------------------------------------
# FIXTURE REPO — two branches, each changing a different file.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
git init -q --initial-branch=main "$REPO"
git -C "$REPO" config user.email "test@spira.local"
git -C "$REPO" config user.name "Spira Test"
touch "$REPO/placeholder"
git -C "$REPO" add placeholder
git -C "$REPO" commit -q -m "initial"
BASE="$(git -C "$REPO" rev-parse HEAD)"

# Branch that changes covered.sh — suite A's glob matches it.
git -C "$REPO" checkout -q -b topic-covered
printf 'changed\n' > "$REPO/covered.sh"
git -C "$REPO" add covered.sh
git -C "$REPO" commit -q -m "change covered.sh"
HEAD_COVERED="$(git -C "$REPO" rev-parse HEAD)"

# Branch that changes a file no suite declares.
git -C "$REPO" checkout -q main
git -C "$REPO" checkout -q -b topic-unmapped
printf 'changed\n' > "$REPO/no-suite-owns-this.txt"
git -C "$REPO" add no-suite-owns-this.txt
git -C "$REPO" commit -q -m "change unmapped file"
HEAD_UNMAPPED="$(git -C "$REPO" rev-parse HEAD)"

# ---------------------------------------------------------------------------
# FIXTURE SUITES
# ---------------------------------------------------------------------------
SD="$TMP/suites"
mkdir -p "$SD"

# Suite A: covers covered.sh — must be selected when covered.sh changes.
cat > "$SD/test-fx-a.sh" << 'EOF'
#!/usr/bin/env bash
# covers: covered.sh
exit 0
EOF

# Suite B: covers other.sh (never touched) — must NOT appear in a targeted diff.
cat > "$SD/test-fx-b.sh" << 'EOF'
#!/usr/bin/env bash
# covers: other.sh
exit 0
EOF

# Suite C: no # covers: line — always runs.
cat > "$SD/test-fx-c.sh" << 'EOF'
#!/usr/bin/env bash
exit 0
EOF

chmod +x "$SD"/test-fx-*.sh

# ---------------------------------------------------------------------------
echo
echo "Part A: --all mode"
# ---------------------------------------------------------------------------

out="$(bash "$SELECT" --all --suite-dir "$SD" 2>/dev/null)"
rc=$?
iszero  "A1: --all exits 0" "$rc"
want    "A1: --all includes test-fx-a.sh" "test-fx-a.sh" "$out"
want    "A1: --all includes test-fx-b.sh" "test-fx-b.sh" "$out"
want    "A1: --all includes test-fx-c.sh" "test-fx-c.sh" "$out"

mf="$TMP/mode-a"
bash "$SELECT" --all --suite-dir "$SD" --mode-file "$mf" >/dev/null 2>&1
iseq "A2: --all writes mode=all" "$(cat "$mf" 2>/dev/null)" "all"

# ---------------------------------------------------------------------------
echo
echo "Part B: diff mode — covered change"
# ---------------------------------------------------------------------------

out="$(bash "$SELECT" --base "$BASE" --head "$HEAD_COVERED" \
          --repo "$REPO" --suite-dir "$SD" 2>/dev/null)"
rc=$?
iszero  "B1: covered diff exits 0" "$rc"
want    "B1: covering suite A selected"     "test-fx-a.sh" "$out"
want    "B1: always-run suite C selected"   "test-fx-c.sh" "$out"
notwant "B1: unrelated suite B not selected" "test-fx-b.sh" "$out"

mf="$TMP/mode-b"
bash "$SELECT" --base "$BASE" --head "$HEAD_COVERED" \
    --repo "$REPO" --suite-dir "$SD" --mode-file "$mf" >/dev/null 2>&1
iseq "B2: covered diff writes mode=diff" "$(cat "$mf" 2>/dev/null)" "diff"

# ---------------------------------------------------------------------------
echo
echo "Part C: diff mode — unmapped change (law-absence-needs-a-positive-control)"
# ---------------------------------------------------------------------------

out="$(bash "$SELECT" --base "$BASE" --head "$HEAD_UNMAPPED" \
          --repo "$REPO" --suite-dir "$SD" 2>/dev/null)"
rc=$?
iszero "C1: unmapped fallback exits 0" "$rc"
want   "C1: fallback includes test-fx-a.sh" "test-fx-a.sh" "$out"
want   "C1: fallback includes test-fx-b.sh" "test-fx-b.sh" "$out"
want   "C1: fallback includes test-fx-c.sh" "test-fx-c.sh" "$out"

mf="$TMP/mode-c"
bash "$SELECT" --base "$BASE" --head "$HEAD_UNMAPPED" \
    --repo "$REPO" --suite-dir "$SD" --mode-file "$mf" >/dev/null 2>&1
iseq "C2: unmapped fallback writes mode=all" "$(cat "$mf" 2>/dev/null)" "all"

# ---------------------------------------------------------------------------
echo
echo "Part D: diff mode — no changed files"
# ---------------------------------------------------------------------------

# Same ref for base and head produces an empty diff.
out="$(bash "$SELECT" --base "$BASE" --head "$BASE" \
          --repo "$REPO" --suite-dir "$SD" 2>/dev/null)"
rc=$?
iszero  "D1: empty diff exits 0" "$rc"
want    "D1: always-run suite C selected"      "test-fx-c.sh" "$out"
notwant "D1: suite A not selected (empty diff)" "test-fx-a.sh" "$out"
notwant "D1: suite B not selected (empty diff)" "test-fx-b.sh" "$out"

mf="$TMP/mode-d"
bash "$SELECT" --base "$BASE" --head "$BASE" \
    --repo "$REPO" --suite-dir "$SD" --mode-file "$mf" >/dev/null 2>&1
iseq "D2: empty diff writes mode=diff" "$(cat "$mf" 2>/dev/null)" "diff"

# ---------------------------------------------------------------------------
echo
echo "Part E: caller contract — no inline loop in either caller"
# ---------------------------------------------------------------------------

# E1: gate-spira.sh does not call suite_covers_of (an inline loop would need it).
# grep -c exits 1 with count "0" when no matches; use || true to suppress the
# non-zero exit without appending a second "0" to the captured output.
_n="$(grep -c 'suite_covers_of' "$HERE/gate-spira.sh" 2>/dev/null || true)"
iseq "E1: gate-spira.sh has no suite_covers_of" "${_n:-0}" "0"

# E2: testenv-batch.sh does not contain the old _cv_all corpus-build variable.
_n="$(grep -c '_cv_all' "$HERE/testenv-batch.sh" 2>/dev/null || true)"
iseq "E2: testenv-batch.sh has no _cv_all (inline loop removed)" "${_n:-0}" "0"

# E3: testenv-batch.sh calls select.sh.
_n="$(grep -c 'select\.sh' "$HERE/testenv-batch.sh" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "E3: testenv-batch.sh calls select.sh" \
    || bad "E3: testenv-batch.sh calls select.sh" "no reference found"

# E4: gate-spira.sh calls select.sh.
_n="$(grep -c 'select\.sh' "$HERE/gate-spira.sh" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "E4: gate-spira.sh calls select.sh" \
    || bad "E4: gate-spira.sh calls select.sh" "no reference found"

# E5: testenv-batch.sh calls select.sh with --no-all-fallback (keeps gate cheap;
#     timed runner gate-spira.sh omits the flag and keeps the full fallback).
_n="$(grep -c 'no-all-fallback' "$HERE/testenv-batch.sh" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "E5: testenv-batch.sh uses --no-all-fallback" \
    || bad "E5: testenv-batch.sh uses --no-all-fallback" "no reference found"

# E6: gate-spira.sh does NOT use --no-all-fallback (it keeps the full fallback).
_n="$(grep -c 'no-all-fallback' "$HERE/gate-spira.sh" 2>/dev/null || true)"
iseq "E6: gate-spira.sh does not use --no-all-fallback (keeps full fallback)" "${_n:-0}" "0"

# ---------------------------------------------------------------------------
echo
echo "Part F: --no-all-fallback mode — unmapped file does not trigger all-suites"
# ---------------------------------------------------------------------------

# F1: unmapped change with --no-all-fallback: only covered+nocov suites, not all
out="$(bash "$SELECT" --base "$BASE" --head "$HEAD_UNMAPPED" \
          --repo "$REPO" --suite-dir "$SD" --no-all-fallback 2>/dev/null)"
rc=$?
iszero  "F1: --no-all-fallback unmapped exits 0"   "$rc"
notwant "F1: suite A not selected (not covered)"   "test-fx-a.sh" "$out"
notwant "F1: suite B not selected (not covered)"   "test-fx-b.sh" "$out"
want    "F1: always-run suite C still selected"    "test-fx-c.sh" "$out"

# F2: --no-all-fallback mode-file writes "diff" (not "all")
mf="$TMP/mode-f"
bash "$SELECT" --base "$BASE" --head "$HEAD_UNMAPPED" \
    --repo "$REPO" --suite-dir "$SD" --no-all-fallback \
    --mode-file "$mf" >/dev/null 2>&1
iseq "F2: --no-all-fallback writes mode=diff" "$(cat "$mf" 2>/dev/null)" "diff"

# ---------------------------------------------------------------------------
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
