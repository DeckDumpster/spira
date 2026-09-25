#!/usr/bin/env bash
# test-held.sh — held.sh reports branches waiting on a human in land=hold repos.
#
# FOUR PROPERTIES UNDER TEST:
#
#   1. CLASSIFICATION: HELD (ahead > 0, bead exists), EMPTY (ahead == 0, bead exists),
#      and ORPHAN (no bead in store) are each labelled correctly in the table output.
#      Positive control: fixture plants one branch of each kind; the table must name all three.
#
#   2. NO-REMOTE HEADER: a repo with no git remote prints "NO REMOTE" unconditionally,
#      before any branch lines.
#
#   3. SUMMARY MODE: --summary emits a HOLD line when branches are held; silent when none.
#
#   4. FAIL CLOSED: a branch whose bead cannot be read (bd itself refuses, not merely a
#      bead the store has never heard of) is reported UNKNOWN, never downgraded to ORPHAN,
#      and held.sh exits non-zero (law-a-control-that-cannot-check-must-refuse).
#
# A REAL bd ON A THROWAWAY DATABASE and a real git repo in a temp dir
# (law-prefer-the-real-dependency, law-a-regression-test-must-be-seen-to-fail).
#
# defect: sp-v4f42, sp-f84wv
# covers: spira/held.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s\n' "$1"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1: wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in output"; }
isz()    { [ "$2" -eq 0 ] && ok "$1" || bad "$1: wanted exit 0, got $2"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-held
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'exit 143' INT TERM
testdb_up held || { printf 'test-held: could not build a fixture database\n' >&2; exit 1; }

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such-conf"
export SPIRA_GOAL=sp-goal

printf 'test-held.sh\n'

# ---------------------------------------------------------------------------
# Build a no-remote fixture git repo with three kinds of spira/* branches.
# Branch names use tst- IDs that cannot match any real bead in the store.
# ---------------------------------------------------------------------------
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email "test@example.com"
git -C "$REPO" config user.name "Test"
git -C "$REPO" commit --allow-empty -m "root"
BASE_BR="$(git -C "$REPO" symbolic-ref --short HEAD)"

# HELD: one commit ahead — bead exists, awaiting human.
git -C "$REPO" checkout -q -b spira/tst-held
git -C "$REPO" commit --allow-empty -m "held work"
git -C "$REPO" checkout -q "$BASE_BR"

# EMPTY: no commits ahead — bead exists, safe to drop.
git -C "$REPO" branch spira/tst-empty

# ORPHAN: one commit ahead — no bead in store, more dangerous to drop.
# Uses tst- prefix so bd show returns not-found without any real-db match.
git -C "$REPO" checkout -q -b spira/tst-orphan
git -C "$REPO" commit --allow-empty -m "orphan work"
git -C "$REPO" checkout -q "$BASE_BR"

export SPIRA_REPO_MAP="$TMP/repo-map"
# Six-column row: name | path | land | base | format | gate
printf 'fixture | %s | hold | | |\n' "$REPO" > "$SPIRA_REPO_MAP"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-held","title":"held bead","status":"closed","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-empty","title":"empty bead","status":"closed","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

# ===========================================================================
# SECTION 1 — positive controls: scanner can find each verdict.
# These checks prove the matcher works; later silence on a clean fixture means
# something (law-absence-needs-a-positive-control).
# ===========================================================================
printf '\npositive controls — scanner classifies each kind:\n'

tbl="$(bash "$HERE/held.sh" 2>&1)"; rc=$?
isz "held.sh exits 0" "$rc"

want "table shows NO REMOTE header"        "NO REMOTE"          "$tbl"
want "table shows HELD verdict"            "HELD — awaiting you" "$tbl"
want "table shows EMPTY verdict"           "EMPTY — safe to drop" "$tbl"
want "table shows ORPHAN verdict"          "ORPHAN — bead is gone" "$tbl"
want "table shows branch column header"    "branch"             "$tbl"
want "table shows verdict column header"   "verdict"            "$tbl"
want "table shows merge hint"              "held.sh --merge"    "$tbl"
want "table shows drop hint"               "held.sh --drop-empty" "$tbl"
want "table shows commit count"            "commit"             "$tbl"

# tst-orphan must appear in the table.
want "table names tst-orphan branch"       "spira/tst-orphan"   "$tbl"

# ===========================================================================
# SECTION 2 — summary mode.
# ===========================================================================
printf '\nsummary mode:\n'

sum="$(bash "$HERE/held.sh" --summary 2>&1)"; rc=$?
isz "summary exits 0" "$rc"
want "summary contains HOLD"             "HOLD"            "$sum"
want "summary contains no remote"        "no remote"       "$sum"
want "summary contains awaiting you"     "awaiting you"    "$sum"

# ===========================================================================
# SECTION 3 — empty fixture: summary is silent.
# ===========================================================================
printf '\nsummary is silent when nothing is held:\n'

REPO2="$TMP/repo2"
git init -q "$REPO2"
git -C "$REPO2" config user.email "test@example.com"
git -C "$REPO2" config user.name "Test"
git -C "$REPO2" commit --allow-empty -m "root"

printf 'empty2 | %s | hold | | |\n' "$REPO2" > "$SPIRA_REPO_MAP"

sum2="$(bash "$HERE/held.sh" --summary 2>&1)"
nowant "summary is silent when no branches" "HOLD" "$sum2"

# ===========================================================================
# SECTION 4 — named repo argument: only that repo is shown.
# ===========================================================================
printf '\nnamed repo argument:\n'

# Restore fixture repo-map with both repos.
printf 'fixture | %s | hold | | |\nfixture2 | %s | hold | | |\n' \
    "$REPO" "$REPO2" > "$SPIRA_REPO_MAP"

tbl_named="$(bash "$HERE/held.sh" fixture 2>&1)"
want "named arg shows fixture"           "fixture"         "$tbl_named"
want "named arg shows HELD verdict"      "HELD"            "$tbl_named"

# ===========================================================================
# SECTION 5 — bd unreadable: UNKNOWN, never ORPHAN, exit non-zero (sp-f84wv).
# SPIRA_BD is the seam lib.sh documents for exactly this: a stub that fails for one
# bead id and delegates every other call to the real bd, so only that branch's lookup
# is affected.
# ===========================================================================
printf '\nbd unreadable -> UNKNOWN, never ORPHAN, exit non-zero:\n'

git -C "$REPO" checkout -q -b spira/tst-unknown
git -C "$REPO" commit --allow-empty -m "unknown work"
git -C "$REPO" checkout -q "$BASE_BR"

REAL_BD="$(command -v bd)"
STUB_BD="$TMP/bd-stub"
cat > "$STUB_BD" <<EOF
#!/usr/bin/env bash
for a in "\$@"; do
    [ "\$a" = "tst-unknown" ] && { printf 'bd-stub: simulated store failure\n' >&2; exit 1; }
done
exec "$REAL_BD" "\$@"
EOF
chmod +x "$STUB_BD"

# Positive control: with a working bd, this same never-seeded id is legitimately ORPHAN —
# proving the matcher can tell "gone" from "unreadable" apart (law-absence-needs-a-positive-control).
baseline_line="$(bash "$HERE/held.sh" fixture 2>&1 | grep 'tst-unknown')"
want "tst-unknown is ORPHAN when bd works and the bead is absent" "ORPHAN" "$baseline_line"

unk_tbl="$(SPIRA_BD="$STUB_BD" bash "$HERE/held.sh" fixture 2>&1)"; unk_rc=$?
if [ "$unk_rc" -ne 0 ]; then ok "held.sh exits non-zero when bd is unreadable"
else bad "held.sh exits non-zero when bd is unreadable: got exit 0"; fi

unk_line="$(printf '%s\n' "$unk_tbl" | grep 'tst-unknown')"
want   "tst-unknown row shows UNKNOWN when bd fails"          "UNKNOWN" "$unk_line"
nowant "tst-unknown row is never reported ORPHAN when bd fails" "ORPHAN" "$unk_line"
want "table names the unknown count" "unknown — bd could not be read" "$unk_tbl"

# ===========================================================================
# SUMMARY
# ===========================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] && exit 0 || exit 1
