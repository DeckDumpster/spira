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
# covers: spira/select.sh spira/select-globs.sh spira/gate-spira.sh spira/testenv-batch.sh spira/gate-touched.sh
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
printf 'changed\n' > "$REPO/no-suite-owns-this.go"
git -C "$REPO" add no-suite-owns-this.go
git -C "$REPO" commit -q -m "change unmapped file"
HEAD_UNMAPPED="$(git -C "$REPO" rev-parse HEAD)"

# Branch that changes Makefile — claimed by suite D's covers:, but plumbing.
git -C "$REPO" checkout -q main
git -C "$REPO" checkout -q -b topic-plumbing
printf 'changed\n' > "$REPO/Makefile"
git -C "$REPO" add Makefile
git -C "$REPO" commit -q -m "change Makefile"
HEAD_PLUMBING="$(git -C "$REPO" rev-parse HEAD)"

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

# Suite D: covers Makefile — a suite that DOES claim the plumbing file. The
# sp-nfiop gap was not an unclaimed file (Part C already caught that); it was
# a claimed file whose claiming suite was the wrong one. Part Q must prove the
# fallback fires even though this suite's covers: line matches.
cat > "$SD/test-fx-d.sh" << 'EOF'
#!/usr/bin/env bash
# covers: Makefile
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

# E7: gate-spira.sh calls select.sh with --files (uses pre-computed list from gate.sh,
#     not --base/--head). This keeps the interface compatible with test fixtures that
#     supply SPIRA_GATE_FILES without git refs.
_n="$(grep -c -- '--files' "$HERE/gate-spira.sh" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "E7: gate-spira.sh calls select.sh with --files" \
    || bad "E7: gate-spira.sh calls select.sh with --files" "no reference found"

# E8: gate-touched.sh (the landing gate selector) calls select.sh — closing the
#     "caller the source never names" gap (law-bake-rules-into-tools).
_n="$(grep -c 'select\.sh' "$HERE/gate-touched.sh" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "E8: gate-touched.sh calls select.sh (ONE selector)" \
    || bad "E8: gate-touched.sh calls select.sh" "no reference found"

# ---------------------------------------------------------------------------
echo
echo "Part F: --no-all-fallback mode — unmapped file does not trigger all-suites"
# ---------------------------------------------------------------------------
# (Note: F tests use --base/--head; G tests mirror them with --files)

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
echo "Part G: --files mode — pre-computed file list replaces git diff"
# ---------------------------------------------------------------------------
# G tests mirror B and C but supply the file list via --files instead of
# --base/--head. The selection result must be identical, proving that the
# two input forms are interchangeable.

# POSITIVE CONTROL: the same file that B proved selects suite A.
FLIST_COVERED="$TMP/flist-covered"
printf 'covered.sh\n' > "$FLIST_COVERED"

out="$(bash "$SELECT" --files "$FLIST_COVERED" --suite-dir "$SD" 2>/dev/null)"
rc=$?
iszero  "G1: --files covered exits 0"          "$rc"
want    "G1: covering suite A selected"         "test-fx-a.sh" "$out"
want    "G1: always-run suite C selected"       "test-fx-c.sh" "$out"
notwant "G1: unrelated suite B not selected"    "test-fx-b.sh" "$out"

# UNMAPPED FILE: mirrors C — triggers all-suites fallback.
FLIST_UNMAPPED="$TMP/flist-unmapped"
printf 'no-suite-owns-this.go\n' > "$FLIST_UNMAPPED"

out="$(bash "$SELECT" --files "$FLIST_UNMAPPED" --suite-dir "$SD" 2>/dev/null)"
rc=$?
iszero "G2: --files unmapped fallback exits 0" "$rc"
want   "G2: fallback includes test-fx-a.sh"    "test-fx-a.sh" "$out"
want   "G2: fallback includes test-fx-b.sh"    "test-fx-b.sh" "$out"
want   "G2: fallback includes test-fx-c.sh"    "test-fx-c.sh" "$out"

# --no-all-fallback with --files: unmapped file does not expand selection.
out="$(bash "$SELECT" --files "$FLIST_UNMAPPED" --suite-dir "$SD" --no-all-fallback 2>/dev/null)"
rc=$?
iszero  "G3: --files --no-all-fallback exits 0"           "$rc"
notwant "G3: suite A not selected (not covered)"          "test-fx-a.sh" "$out"
notwant "G3: suite B not selected (not covered)"          "test-fx-b.sh" "$out"
want    "G3: always-run suite C still selected"           "test-fx-c.sh" "$out"

# mode-file with --files: covered → "diff", unmapped fallback → "all"
mf="$TMP/mode-g-covered"
bash "$SELECT" --files "$FLIST_COVERED" --suite-dir "$SD" \
    --mode-file "$mf" >/dev/null 2>&1
iseq "G4: --files covered writes mode=diff" "$(cat "$mf" 2>/dev/null)" "diff"

mf="$TMP/mode-g-unmapped"
bash "$SELECT" --files "$FLIST_UNMAPPED" --suite-dir "$SD" \
    --mode-file "$mf" >/dev/null 2>&1
iseq "G5: --files unmapped writes mode=all" "$(cat "$mf" 2>/dev/null)" "all"

# ---------------------------------------------------------------------------
echo
echo "Part H: --report-file — unclaimed and unplaced reporting"
# ---------------------------------------------------------------------------

# Shared fixture: two source files, one covered, one not.
# This is the positive control for unclaimed detection (law-absence-needs-a-positive-control).
SD_H="$TMP/suites-h"
REPO_H="$TMP/repo-h"
mkdir -p "$SD_H"
git init -q --initial-branch=main "$REPO_H"
git -C "$REPO_H" config user.email "test@spira.local"
git -C "$REPO_H" config user.name "Spira Test"
printf '#!/bin/bash\necho covered\n' > "$REPO_H/util.sh"
printf '#!/bin/bash\necho orphan\n' > "$REPO_H/orphan.sh"
git -C "$REPO_H" add util.sh orphan.sh
git -C "$REPO_H" commit -q -m "add source files"

# Suite that covers util.sh but NOT orphan.sh.
cat > "$SD_H/test-fx-h.sh" << 'EOF'
#!/usr/bin/env bash
# covers: util.sh
exit 0
EOF
chmod +x "$SD_H/test-fx-h.sh"

# H1: Fixture with an unclaimed source file — report is non-empty.
RF_H1="$TMP/report-h1"
bash "$SELECT" --all --suite-dir "$SD_H" --repo "$REPO_H" \
    --report-file "$RF_H1" >/dev/null 2>&1
rc=$?
iszero "H1: fixture with unclaimed file exits 0" "$rc"
_n="$(grep -c 'unclaimed:' "$RF_H1" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "H1: unclaimed source file appears in report" \
    || bad "H1: unclaimed source file appears in report" \
           "expected >=1 unclaimed, got ${_n:-0}"
want "H1: orphan.sh named as unclaimed" "unclaimed:orphan.sh" \
    "$(cat "$RF_H1" 2>/dev/null)"

# H2: Same fixture but with orphan.sh covered — report is empty.
cat > "$SD_H/test-fx-h2.sh" << 'EOF'
#!/usr/bin/env bash
# covers: orphan.sh
exit 0
EOF
chmod +x "$SD_H/test-fx-h2.sh"

RF_H2="$TMP/report-h2"
bash "$SELECT" --all --suite-dir "$SD_H" --repo "$REPO_H" \
    --report-file "$RF_H2" >/dev/null 2>&1
rc=$?
iszero "H2: fixture all-covered exits 0" "$rc"
_n="$(grep -c 'unclaimed:' "$RF_H2" 2>/dev/null || true)"
iseq "H2: fixture all-covered has no unclaimed entries" "${_n:-0}" "0"

# POSITIVE CONTROL: remove the second suite — orphan.sh becomes unclaimed again.
rm "$SD_H/test-fx-h2.sh"
RF_H2B="$TMP/report-h2b"
bash "$SELECT" --all --suite-dir "$SD_H" --repo "$REPO_H" \
    --report-file "$RF_H2B" >/dev/null 2>&1
_n="$(grep -c 'unclaimed:' "$RF_H2B" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "H2-ctrl: removing coverage adds unclaimed entry" \
    || bad "H2-ctrl: removing coverage adds unclaimed entry" \
           "expected >=1 unclaimed, got ${_n:-0}"

# H3: Unplaced changed file appears as unplaced: entry in the report.
# FLIST_UNMAPPED (no-suite-owns-this.txt) is from Part G; SD has no suite covering it.
# Use --no-all-fallback so selection still exits 0 without running everything.
RF_H3="$TMP/report-h3"
bash "$SELECT" --files "$FLIST_UNMAPPED" --suite-dir "$SD" --no-all-fallback \
    --report-file "$RF_H3" >/dev/null 2>&1
rc=$?
iszero "H3: unplaced file exits 0" "$rc"
want "H3: unplaced file named in report" "no-suite-owns-this.go" \
    "$(cat "$RF_H3" 2>/dev/null)"

# H4: --report-file not given — exits 0 with no side effects.
bash "$SELECT" --all --suite-dir "$SD" >/dev/null 2>&1
iszero "H4: no --report-file exits 0" "$?"

# ---------------------------------------------------------------------------
echo
echo "Part I: inert file classification — inert changes select nothing"
# ---------------------------------------------------------------------------
# Fixture: one suite covers some.sh; the changed file is README.md (inert).
SD_I="$TMP/suites-i"
mkdir -p "$SD_I"
cat > "$SD_I/test-fx-i.sh" << 'EOF'
#!/usr/bin/env bash
# covers: some.sh
exit 0
EOF
chmod +x "$SD_I/test-fx-i.sh"

FLIST_INERT="$TMP/flist-inert"
printf 'README.md\n' > "$FLIST_INERT"

# POSITIVE CONTROL: without SPIRA_SELECT_INERT suppressing *.md, README.md is
# unmapped and triggers the all-suites fallback — this proves the file is noticed.
out_ctrl="$(SPIRA_SELECT_INERT='__none__' bash "$SELECT" \
    --files "$FLIST_INERT" --suite-dir "$SD_I" 2>/dev/null)"
want "I0-ctrl: without SELECT_INERT, README.md triggers fallback (test-fx-i.sh appears)" \
    "test-fx-i.sh" "$out_ctrl"

# I1: README.md declared inert → nothing selected (no suites, not even always-run).
out="$(SPIRA_SELECT_INERT='*.md *.txt' bash "$SELECT" \
    --files "$FLIST_INERT" --suite-dir "$SD_I" 2>/dev/null)"
rc=$?
iszero "I1: inert file exits 0"         "$rc"
iseq   "I1: inert file selects nothing" "$out" ""

# I2: *.txt also inert — CHANGES.txt selects nothing.
printf 'CHANGES.txt\n' > "$TMP/flist-txt"
out="$(SPIRA_SELECT_INERT='*.md *.txt' bash "$SELECT" \
    --files "$TMP/flist-txt" --suite-dir "$SD_I" 2>/dev/null)"
rc=$?
iszero "I2: .txt file exits 0"         "$rc"
iseq   "I2: .txt file selects nothing" "$out" ""

# ---------------------------------------------------------------------------
echo
echo "Part J: unclaimed source file — exits non-zero and names the file"
# ---------------------------------------------------------------------------
# Fixture: one suite covers other.sh; the changed file is new.sh (source, unclaimed).
SD_J="$TMP/suites-j"
mkdir -p "$SD_J"
cat > "$SD_J/test-fx-j.sh" << 'EOF'
#!/usr/bin/env bash
# covers: other.sh
exit 0
EOF
chmod +x "$SD_J/test-fx-j.sh"

FLIST_SRC="$TMP/flist-src"
printf 'new.sh\n' > "$FLIST_SRC"

# POSITIVE CONTROL: without SELECT_SOURCE matching *.sh, new.sh is just an
# unknown unmapped file and triggers the all-suites fallback at exit 0 — this
# proves the file is in scope before the source check fires.
out_ctrl="$(SPIRA_SELECT_SOURCE='__none__' bash "$SELECT" \
    --files "$FLIST_SRC" --suite-dir "$SD_J" 2>/dev/null)"
rc_ctrl=$?
iszero "J0-ctrl: without SELECT_SOURCE, unclaimed .sh exits 0" "$rc_ctrl"
want   "J0-ctrl: without SELECT_SOURCE, fallback selects test-fx-j.sh" \
    "test-fx-j.sh" "$out_ctrl"

# J1: new.sh declared as source → exits non-zero and names the file.
err="$(SPIRA_SELECT_SOURCE='*.sh' bash "$SELECT" \
    --files "$FLIST_SRC" --suite-dir "$SD_J" 2>&1 >/dev/null)"
rc=$?
[ "$rc" -ne 0 ] && ok "J1: unclaimed source file exits non-zero" \
    || bad "J1: unclaimed source file exits non-zero" "got exit $rc"
want "J1: error names the unclaimed file" "new.sh" "$err"

# J2: add a suite that claims new.sh → exits 0.
cat > "$SD_J/test-fx-j2.sh" << 'EOF'
#!/usr/bin/env bash
# covers: new.sh
exit 0
EOF
chmod +x "$SD_J/test-fx-j2.sh"

out="$(SPIRA_SELECT_SOURCE='*.sh' bash "$SELECT" \
    --files "$FLIST_SRC" --suite-dir "$SD_J" 2>/dev/null)"
rc=$?
iszero "J2: claimed source file exits 0" "$rc"
want   "J2: claiming suite selected"     "test-fx-j2.sh" "$out"

# J2-ctrl: remove the claiming suite → error returns.
rm "$SD_J/test-fx-j2.sh"
SPIRA_SELECT_SOURCE='*.sh' bash "$SELECT" \
    --files "$FLIST_SRC" --suite-dir "$SD_J" >/dev/null 2>/dev/null
rc=$?
[ "$rc" -ne 0 ] && ok "J2-ctrl: removing coverage triggers error again" \
    || bad "J2-ctrl: removing coverage triggers error again" "got exit $rc"

# ---------------------------------------------------------------------------
echo
echo "Part K: gate-touched.sh acceptance — landing gate uses select.sh for # covers:"
# ---------------------------------------------------------------------------
# K1 (static): gate-touched.sh calls select.sh — the landing gate selector is the ONE selector.
TOUCHED="$HERE/gate-touched.sh"
_n="$(grep -c 'select\.sh' "$TOUCHED" 2>/dev/null || true)"
[ "${_n:-0}" -ge 1 ] && ok "K1: gate-touched.sh calls select.sh" \
    || bad "K1: gate-touched.sh calls select.sh" "no reference found"

# K2 (functional): SPIRA_GATE_FILES containing spira/conf.sh → test-aeon-world-stop.sh selected.
# Positive control (law-absence-needs-a-positive-control): the old gate-touched.sh (git diff on
# test-*.sh only) would miss test-aeon-world-stop.sh since it isn't the file that changed.
# The new gate-touched.sh reads # covers: via select.sh and selects it.
FLIST_CONF="$TMP/flist-conf"
printf 'spira/conf.sh\n' > "$FLIST_CONF"
out_k="$(SPIRA_GATE_FILES="$FLIST_CONF" bash "$TOUCHED" dummy-base dummy-head 2>/dev/null)"
want "K2: conf.sh change selects test-aeon-world-stop.sh (acceptance criterion)" \
    "test-aeon-world-stop.sh" "$out_k"

# K3 (fixture): SPIRA_GATE_FILES with covered.sh selects the covering suite, using
# the fixture SUITE_DIR from Part B. Isolates from real suite declarations.
FLIST_K="$TMP/flist-k"
printf 'covered.sh\n' > "$FLIST_K"
out_k3="$(SPIRA_GATE_FILES="$FLIST_K" SPIRA_BATCH_SUITE_DIR="$SD" \
    bash "$TOUCHED" dummy-base dummy-head 2>/dev/null)"
want    "K3: fixture: covered file selects its suite"    "test-fx-a.sh" "$out_k3"
notwant "K3: fixture: uncovered suite not selected"      "test-fx-b.sh" "$out_k3"

# ---------------------------------------------------------------------------
echo
echo "Part L: docs-only diff — inert files select nothing; pair: unmapped .sh selects all"
# ---------------------------------------------------------------------------
# This verifies the SELECT_INERT filter: docs/*.md changes are inert and never
# trigger the all-suites fallback. A genuinely unmapped .sh does.

SD_L="$TMP/suites-l"
mkdir -p "$SD_L"
cat > "$SD_L/test-fx-l.sh" << 'EOF'
#!/usr/bin/env bash
# covers: some.sh
exit 0
EOF
# Always-run suite (no covers line)
cat > "$SD_L/test-fx-l-nocov.sh" << 'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SD_L"/test-fx-l*.sh

FLIST_DOCS="$TMP/flist-docs"
printf 'docs/spikes/notes.md\n' > "$FLIST_DOCS"

# L1: docs-only diff → only always-run suite (no fallback, no coverage suites)
out="$(SPIRA_SELECT_INERT='*.md *.txt' bash "$SELECT" \
    --files "$FLIST_DOCS" --suite-dir "$SD_L" 2>/dev/null)"
rc=$?
iszero  "L1: docs-only diff exits 0"                   "$rc"
notwant "L1: coverage suite not selected"              "test-fx-l.sh" "$out"
want    "L1: always-run suite still selected"          "test-fx-l-nocov.sh" "$out"

# L1-ctrl: unmapped .sh with the same suite dir → all-suites fallback
FLIST_UNMAPPED_SH="$TMP/flist-unmapped-sh"
printf 'lib-nobody-covers.sh\n' > "$FLIST_UNMAPPED_SH"

out="$(SPIRA_SELECT_INERT='*.md *.txt' bash "$SELECT" \
    --files "$FLIST_UNMAPPED_SH" --suite-dir "$SD_L" 2>/dev/null)"
rc=$?
iszero "L1-ctrl: unmapped .sh exits 0" "$rc"
want   "L1-ctrl: fallback includes coverage suite"   "test-fx-l.sh"       "$out"
want   "L1-ctrl: fallback includes always-run suite" "test-fx-l-nocov.sh" "$out"

# ---------------------------------------------------------------------------
echo
echo "Part M: function-level coverage — hub file hunk selects only matching function suite"
# ---------------------------------------------------------------------------
# Suite M-A covers lib-hub.sh#foo, M-B covers lib-hub.sh#bar.
# A diff that touches only function foo selects M-A but not M-B.
# A diff with a hunk outside any declared function selects both (fallback within file).

REPO_M="$TMP/repo-m"
git init -q --initial-branch=main "$REPO_M"
git -C "$REPO_M" config user.email "test@spira.local"
git -C "$REPO_M" config user.name "Spira Test"
# Initial lib-hub.sh with two functions
cat > "$REPO_M/lib-hub.sh" << 'LIBEOF'
#!/usr/bin/env bash
foo() {
    echo "foo v1"
}

bar() {
    echo "bar v1"
}
LIBEOF
git -C "$REPO_M" add lib-hub.sh
git -C "$REPO_M" commit -q -m "initial"
BASE_M="$(git -C "$REPO_M" rev-parse HEAD)"

# Branch 1: change only function foo
git -C "$REPO_M" checkout -q -b topic-foo
cat > "$REPO_M/lib-hub.sh" << 'LIBEOF'
#!/usr/bin/env bash
foo() {
    echo "foo v2"
}

bar() {
    echo "bar v1"
}
LIBEOF
git -C "$REPO_M" add lib-hub.sh
git -C "$REPO_M" commit -q -m "change foo"
HEAD_FOO="$(git -C "$REPO_M" rev-parse HEAD)"

# Branch 2: change outside any declared function (top-level comment/code)
git -C "$REPO_M" checkout -q main
git -C "$REPO_M" checkout -q -b topic-global
cat > "$REPO_M/lib-hub.sh" << 'LIBEOF'
#!/usr/bin/env bash
# global comment changed
foo() {
    echo "foo v1"
}

bar() {
    echo "bar v1"
}
LIBEOF
git -C "$REPO_M" add lib-hub.sh
git -C "$REPO_M" commit -q -m "change global"
HEAD_GLOBAL="$(git -C "$REPO_M" rev-parse HEAD)"

SD_M="$TMP/suites-m"
mkdir -p "$SD_M"
# Suite MA covers lib-hub.sh#foo only
cat > "$SD_M/test-fx-ma.sh" << 'EOF'
#!/usr/bin/env bash
# covers: lib-hub.sh#foo
exit 0
EOF
# Suite MB covers lib-hub.sh#bar only
cat > "$SD_M/test-fx-mb.sh" << 'EOF'
#!/usr/bin/env bash
# covers: lib-hub.sh#bar
exit 0
EOF
chmod +x "$SD_M"/test-fx-m*.sh

# M1: hunk inside function foo → MA selected, MB not selected
out="$(bash "$SELECT" --base "$BASE_M" --head "$HEAD_FOO" \
          --repo "$REPO_M" --suite-dir "$SD_M" 2>/dev/null)"
rc=$?
iszero  "M1: foo-only diff exits 0"                        "$rc"
want    "M1: lib-hub.sh#foo suite selected"                "test-fx-ma.sh" "$out"
notwant "M1: lib-hub.sh#bar suite not selected"            "test-fx-mb.sh" "$out"

# M1-ctrl: hunk outside any declared function → both suites selected (cannot narrow further)
out="$(bash "$SELECT" --base "$BASE_M" --head "$HEAD_GLOBAL" \
          --repo "$REPO_M" --suite-dir "$SD_M" 2>/dev/null)"
rc=$?
iszero "M1-ctrl: global-scope diff exits 0"                "$rc"
want   "M1-ctrl: MA selected when hunk outside functions"  "test-fx-ma.sh" "$out"
want   "M1-ctrl: MB selected when hunk outside functions"  "test-fx-mb.sh" "$out"

# M2: --files mode treats file#func as file-glob (conservative: always select when file present)
FLIST_M="$TMP/flist-m"
printf 'lib-hub.sh\n' > "$FLIST_M"
out="$(bash "$SELECT" --files "$FLIST_M" --suite-dir "$SD_M" 2>/dev/null)"
rc=$?
iszero "M2: --files mode with file#func exits 0"           "$rc"
want   "M2: --files mode selects MA (conservative)"        "test-fx-ma.sh" "$out"
want   "M2: --files mode selects MB (conservative)"        "test-fx-mb.sh" "$out"

# ---------------------------------------------------------------------------
echo
echo "Part N: queue-branch union — batch diff selects union; fallback on unmapped member"
# ---------------------------------------------------------------------------
# A queue branch combines multiple topic branches. The diff (base..queue head)
# is the union of all member diffs. The selector runs over this combined diff.
# If any member touches unmapped code, the full corpus fallback fires.

REPO_N="$TMP/repo-n"
git init -q --initial-branch=main "$REPO_N"
git -C "$REPO_N" config user.email "test@spira.local"
git -C "$REPO_N" config user.name "Spira Test"
touch "$REPO_N/placeholder"
git -C "$REPO_N" add placeholder
git -C "$REPO_N" commit -q -m "initial"
BASE_N="$(git -C "$REPO_N" rev-parse HEAD)"

# Merge branch 1 (changes alpha.sh) and branch 2 (changes beta.sh) onto a queue branch
printf 'changed\n' > "$REPO_N/alpha.sh"
git -C "$REPO_N" add alpha.sh
git -C "$REPO_N" commit -q -m "branch1: alpha"
printf 'changed\n' > "$REPO_N/beta.sh"
git -C "$REPO_N" add beta.sh
git -C "$REPO_N" commit -q -m "branch2: beta"
HEAD_N_CLEAN="$(git -C "$REPO_N" rev-parse HEAD)"

# Queue branch also includes an unmapped .go file
printf 'changed\n' > "$REPO_N/unmapped.go"
git -C "$REPO_N" add unmapped.go
git -C "$REPO_N" commit -q -m "branch3: unmapped"
HEAD_N_UNMAPPED="$(git -C "$REPO_N" rev-parse HEAD)"

SD_N="$TMP/suites-n"
mkdir -p "$SD_N"
cat > "$SD_N/test-fx-na.sh" << 'EOF'
#!/usr/bin/env bash
# covers: alpha.sh
exit 0
EOF
cat > "$SD_N/test-fx-nb.sh" << 'EOF'
#!/usr/bin/env bash
# covers: beta.sh
exit 0
EOF
cat > "$SD_N/test-fx-nc.sh" << 'EOF'
#!/usr/bin/env bash
# covers: other.sh
exit 0
EOF
chmod +x "$SD_N"/test-fx-n*.sh

# N1: clean batch diff → union of covering suites (alpha+beta, not other)
out="$(bash "$SELECT" --base "$BASE_N" --head "$HEAD_N_CLEAN" \
          --repo "$REPO_N" --suite-dir "$SD_N" 2>/dev/null)"
rc=$?
iszero  "N1: clean batch diff exits 0"                   "$rc"
want    "N1: alpha.sh suite selected"                    "test-fx-na.sh" "$out"
want    "N1: beta.sh suite selected"                     "test-fx-nb.sh" "$out"
notwant "N1: uncovered suite not selected"               "test-fx-nc.sh" "$out"

# N1-ctrl: batch diff with unmapped member → all-suites fallback
out="$(bash "$SELECT" --base "$BASE_N" --head "$HEAD_N_UNMAPPED" \
          --repo "$REPO_N" --suite-dir "$SD_N" 2>/dev/null)"
rc=$?
iszero "N1-ctrl: batch with unmapped member exits 0"     "$rc"
want   "N1-ctrl: fallback includes all suites (na)"      "test-fx-na.sh" "$out"
want   "N1-ctrl: fallback includes all suites (nb)"      "test-fx-nb.sh" "$out"
want   "N1-ctrl: fallback includes all suites (nc)"      "test-fx-nc.sh" "$out"

# ---------------------------------------------------------------------------
echo
echo "Part O: # selects-on: added,mode — invariant-suite selection by diff event"
# ---------------------------------------------------------------------------
# A suite with # selects-on: added,mode is selected when a file matching its
# # covers: glob is ADDED or has its MODE CHANGED, not just content-modified.
# POSITIVE CONTROL: the suite is selected when the condition fires; the
# pair verifies it is NOT selected for an unrelated diff (law-a-regression-test-must-be-seen-to-fail).

# Fixture: one suite with # selects-on: added,mode on spira/*.sh
SD_O="$TMP/suites-o"
mkdir -p "$SD_O"

cat > "$SD_O/test-fx-o-inv.sh" << 'EOF'
#!/usr/bin/env bash
# covers: spira/*.sh
# selects-on: added,mode
exit 0
EOF

# Suite with only # covers: (no selects-on) — should be selected for content changes.
cat > "$SD_O/test-fx-o-cov.sh" << 'EOF'
#!/usr/bin/env bash
# covers: spira/*.sh
exit 0
EOF

chmod +x "$SD_O"/test-fx-o*.sh

# O1: --files with A<tab>spira/new.sh → selects-on:added fires → test-fx-o-inv.sh selected
FLIST_O_ADDED="$TMP/flist-o-added"
printf 'A\tspira/new.sh\n' > "$FLIST_O_ADDED"

out="$(bash "$SELECT" --files "$FLIST_O_ADDED" --suite-dir "$SD_O" 2>/dev/null)"
rc=$?
iszero  "O1: added file exits 0"                              "$rc"
want    "O1: selects-on:added fires for added file"          "test-fx-o-inv.sh" "$out"
want    "O1: covers-only suite also selected"                 "test-fx-o-cov.sh" "$out"

# O1-ctrl: plain filename (no status) → treated as M → selects-on:added does NOT fire
# but covers-only suite is still selected (spira/new.sh matches spira/*.sh for content match)
FLIST_O_PLAIN="$TMP/flist-o-plain"
printf 'spira/new.sh\n' > "$FLIST_O_PLAIN"

out="$(bash "$SELECT" --files "$FLIST_O_PLAIN" --suite-dir "$SD_O" 2>/dev/null)"
rc=$?
iszero  "O1-ctrl: plain file exits 0"                                  "$rc"
notwant "O1-ctrl: selects-on:added NOT fired for plain (M) file"      "test-fx-o-inv.sh" "$out"
want    "O1-ctrl: covers-only suite selected (content match)"          "test-fx-o-cov.sh" "$out"

# O2: --base/--head with a branch that ADDS spira/new.sh → selects-on:added fires
REPO_O="$TMP/repo-o"
git init -q --initial-branch=main "$REPO_O"
git -C "$REPO_O" config user.email "test@spira.local"
git -C "$REPO_O" config user.name "Spira Test"
touch "$REPO_O/placeholder"
git -C "$REPO_O" add placeholder
git -C "$REPO_O" commit -q -m "initial"
BASE_O="$(git -C "$REPO_O" rev-parse HEAD)"

git -C "$REPO_O" checkout -q -b topic-add-script
# Add spira/new.sh with mode 100644 (NOT executable — the actual defect being guarded)
mkdir -p "$REPO_O/spira"
printf '#!/usr/bin/env bash\ntrue\n' > "$REPO_O/spira/new.sh"
git -C "$REPO_O" add spira/new.sh
git -C "$REPO_O" commit -q -m "add spira/new.sh"
HEAD_O_ADD="$(git -C "$REPO_O" rev-parse HEAD)"

out="$(bash "$SELECT" --base "$BASE_O" --head "$HEAD_O_ADD" \
          --repo "$REPO_O" --suite-dir "$SD_O" 2>/dev/null)"
rc=$?
iszero "O2: branch that adds spira/new.sh exits 0"                      "$rc"
want   "O2: selects-on:added fires for added script"                     "test-fx-o-inv.sh" "$out"

# O2-ctrl: branch that edits README.md (inert) does NOT select the selects-on suite
git -C "$REPO_O" checkout -q main
git -C "$REPO_O" checkout -q -b topic-edit-md
printf '# changed\n' > "$REPO_O/README.md"
git -C "$REPO_O" add README.md
git -C "$REPO_O" commit -q -m "edit README"
HEAD_O_MD="$(git -C "$REPO_O" rev-parse HEAD)"

out="$(SPIRA_SELECT_INERT='*.md *.txt' bash "$SELECT" \
          --base "$BASE_O" --head "$HEAD_O_MD" \
          --repo "$REPO_O" --suite-dir "$SD_O" 2>/dev/null)"
rc=$?
iszero  "O2-ctrl: .md-only diff exits 0"                                     "$rc"
notwant "O2-ctrl: selects-on suite NOT selected for .md edit"                "test-fx-o-inv.sh" "$out"
notwant "O2-ctrl: covers-only suite NOT selected for inert .md edit"         "test-fx-o-cov.sh" "$out"

# O3: --base/--head with a mode change on an existing file
git -C "$REPO_O" checkout -q main
git -C "$REPO_O" checkout -q -b topic-mode-change
mkdir -p "$REPO_O/spira"
printf '#!/usr/bin/env bash\ntrue\n' > "$REPO_O/spira/old.sh"
chmod +x "$REPO_O/spira/old.sh"
git -C "$REPO_O" add spira/old.sh
git -C "$REPO_O" commit -q -m "add old.sh with +x"
BASE_O_MODE="$(git -C "$REPO_O" rev-parse HEAD)"

# Strip execute bit — mode change without content change
chmod -x "$REPO_O/spira/old.sh"
git -C "$REPO_O" add spira/old.sh
git -C "$REPO_O" commit -q -m "remove +x from old.sh"
HEAD_O_MODE="$(git -C "$REPO_O" rev-parse HEAD)"

out="$(bash "$SELECT" --base "$BASE_O_MODE" --head "$HEAD_O_MODE" \
          --repo "$REPO_O" --suite-dir "$SD_O" 2>/dev/null)"
rc=$?
iszero "O3: mode-change branch exits 0"                                  "$rc"
want   "O3: selects-on:mode fires for mode-changed script"               "test-fx-o-inv.sh" "$out"

# ---------------------------------------------------------------------------
echo
echo "Part P: real tree — spira/new.sh (100644) addition selects test-script-exec.sh"
# ---------------------------------------------------------------------------
# Verify that the real test-script-exec.sh (# covers: spira/*.sh, # selects-on: added,mode)
# is selected when a spira/*.sh file is added, using the --files mode with A-status.
# This is the production scenario from the bead: a branch adds a new script without +x.
FLIST_P="$TMP/flist-p"
printf 'A\tspira/new-script.sh\n' > "$FLIST_P"

out_p="$(bash "$SELECT" --files "$FLIST_P" --suite-dir "$HERE" 2>/dev/null)"
rc_p=$?
iszero "P1: added spira script exits 0"                                          "$rc_p"
want   "P1: test-script-exec.sh selected for added spira/new-script.sh"         "test-script-exec.sh" "$out_p"

# Pair: editing an unrelated .md does NOT select test-script-exec.sh
FLIST_P_MD="$TMP/flist-p-md"
printf 'README.md\n' > "$FLIST_P_MD"

out_p_md="$(SPIRA_SELECT_INERT='*.md *.txt' bash "$SELECT" \
    --files "$FLIST_P_MD" --suite-dir "$HERE" 2>/dev/null)"
rc_p_md=$?
iszero  "P2: .md-only diff exits 0"                                                  "$rc_p_md"
notwant "P2: test-script-exec.sh NOT selected for .md edit"                         "test-script-exec.sh" "$out_p_md"

# ---------------------------------------------------------------------------
echo
echo "Part Q: plumbing bucket — a claimed file still forces all-suites fallback (sp-221n8)"
# ---------------------------------------------------------------------------
# sp-nfiop (05d6a633) changed Makefile, spira/skew.sh and systemd/install.sh;
# select.sh selected 47 suites because every one of those files WAS claimed by
# some suite's covers: line — just not by test-install-bootstrap-release.sh or
# test-watch-refresh.sh, the ones that actually exercised the change. A file
# that reaches shared build/install/runtime plumbing cannot be trusted to any
# single suite's covers: claim; PLUMBING forces the full corpus regardless.

# POSITIVE CONTROL: with SELECT_PLUMBING disabled, the claimed Makefile change
# behaves exactly like Part B — only the claiming suite + always-run selected,
# proving PLUMBING (not some other mechanism) is what Q1 below relies on.
out_q_ctrl="$(SPIRA_SELECT_PLUMBING='__none__' bash "$SELECT" \
    --base "$BASE" --head "$HEAD_PLUMBING" --repo "$REPO" --suite-dir "$SD" 2>/dev/null)"
rc_q_ctrl=$?
iszero  "Q0-ctrl: without PLUMBING, claimed Makefile change exits 0" "$rc_q_ctrl"
want    "Q0-ctrl: claiming suite D selected"      "test-fx-d.sh" "$out_q_ctrl"
want    "Q0-ctrl: always-run suite C selected"    "test-fx-c.sh" "$out_q_ctrl"
notwant "Q0-ctrl: unrelated suite B NOT selected" "test-fx-b.sh" "$out_q_ctrl"

out_q="$(bash "$SELECT" --base "$BASE" --head "$HEAD_PLUMBING" \
    --repo "$REPO" --suite-dir "$SD" 2>/dev/null)"
rc_q=$?
iszero "Q1: plumbing change exits 0" "$rc_q"
want   "Q1: fallback includes claiming suite D" "test-fx-d.sh" "$out_q"
want   "Q1: fallback includes unrelated suite B" "test-fx-b.sh" "$out_q"
want   "Q1: fallback includes always-run suite C" "test-fx-c.sh" "$out_q"

mf="$TMP/mode-q"
bash "$SELECT" --base "$BASE" --head "$HEAD_PLUMBING" \
    --repo "$REPO" --suite-dir "$SD" --mode-file "$mf" >/dev/null 2>&1
iseq "Q2: plumbing fallback writes mode=all" "$(cat "$mf" 2>/dev/null)" "all"

# Q3: --no-all-fallback suppresses the plumbing fallback too (same contract as
# the unmapped fallback — a caller that opted into "cheap" accepts the risk).
out_q_nf="$(bash "$SELECT" --base "$BASE" --head "$HEAD_PLUMBING" \
    --repo "$REPO" --suite-dir "$SD" --no-all-fallback 2>/dev/null)"
rc_q_nf=$?
iszero  "Q3: --no-all-fallback plumbing exits 0"        "$rc_q_nf"
want    "Q3: claiming suite D still selected"           "test-fx-d.sh" "$out_q_nf"
notwant "Q3: unrelated suite B not selected"             "test-fx-b.sh" "$out_q_nf"

# ---------------------------------------------------------------------------
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
