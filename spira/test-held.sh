#!/usr/bin/env bash
# test-held.sh — held.sh reports branches waiting on a human in land=hold repos.
#
# FIVE PROPERTIES UNDER TEST:
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
#   4. NAMED-REPO ARGUMENT: a repo argument restricts the table to that repo and excludes
#      every other hold-mode repo's branches.
#
#   5. FAIL CLOSED: a branch whose bead cannot be read (bd itself refuses, not merely a
#      bead the store has never heard of) is reported UNKNOWN, never downgraded to ORPHAN,
#      and held.sh exits non-zero (law-a-control-that-cannot-check-must-refuse).
#
# A REAL git REPO IN A TEMP DIR, and a hand-written bd STUB (SPIRA_BD) instead of a
# throwaway database. held.sh reaches bd through exactly one seam — `bdjson show <id>`,
# called by _bead_status — and that seam's only job is classification (present / absent /
# unreadable), not bd's own filtering or query behaviour. A stub answering that one shape is
# not a model of bd that can drift; it is the seam contract itself
# (law-a-control-that-cannot-check-must-refuse still runs against the real held.sh, only the
# bead lookup is swapped).
#
# defect: sp-v4f42, sp-f84wv
# tier: T2
# covers: spira/held.sh spira/lib.sh
# hermetic-ok: no database, no systemd, no gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isz()    { [ "$2" -eq 0 ] && ok "$1" || bad "$1: wanted exit 0, got $2"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
trap 'exit 143' INT TERM

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such-conf"
# NEVER THE REAL STORE. conf.sh only runs bd's schema check against a $SPIRA_DB that already
# exists on disk — a nonexistent path skips it, and skipping it is what keeps this suite from
# ever touching the operator's own database despite never calling testdb_up at all.
export SPIRA_DB="$TMP/no-such-db"

printf 'test-held.sh\n'

# ---------------------------------------------------------------------------
# The bd stub. held.sh's only bd call is `bdjson show <id>`, issued by _bead_status.
# This answers it from a fixed table: known ids are closed beads, anything else is "[]"
# (bd's own shape for "no such bead"), and one id can be told to fail outright via an env
# toggle the suite sets only for the fail-closed section.
# ---------------------------------------------------------------------------
STUB_BD="$TMP/bd-stub"
cat > "$STUB_BD" <<'EOF'
#!/usr/bin/env bash
id=""; skip_next=0
for a in "$@"; do
    if [ "$skip_next" = 1 ]; then skip_next=0; continue; fi
    case "$a" in
        -C)      skip_next=1 ;;
        show)    ;;
        -*)      ;;
        *)       id="$a" ;;
    esac
done
if [ "$id" = tst-unknown ] && [ "${SPIRA_TEST_BD_FAIL_UNKNOWN:-0}" = 1 ]; then
    printf 'bd-stub: simulated store failure\n' >&2
    exit 1
fi
case "$id" in
    tst-held|tst-empty|tst-r2held) printf '[{"id":"%s","status":"closed"}]\n' "$id" ;;
    *)                             printf '[]\n' ;;
esac
EOF
chmod +x "$STUB_BD"
export SPIRA_BD="$STUB_BD"

# ---------------------------------------------------------------------------
# Build a no-remote fixture git repo with three kinds of spira/* branches.
# Branch names use tst- IDs; the stub above is what decides which ones have beads.
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
git -C "$REPO" checkout -q -b spira/tst-orphan
git -C "$REPO" commit --allow-empty -m "orphan work"
git -C "$REPO" checkout -q "$BASE_BR"

export SPIRA_REPO_MAP="$TMP/repo-map"
# Six-column row: name | path | land | base | format | gate
printf 'fixture | %s | hold | | |\n' "$REPO" > "$SPIRA_REPO_MAP"

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
BASE_BR2="$(git -C "$REPO2" symbolic-ref --short HEAD)"

printf 'empty2 | %s | hold | | |\n' "$REPO2" > "$SPIRA_REPO_MAP"

sum2="$(bash "$HERE/held.sh" --summary 2>&1)"
nowant "summary is silent when no branches" "HOLD" "$sum2"

# ===========================================================================
# SECTION 4 — named repo argument: only that repo is shown, and the other repo's
# branches are excluded, not merely unmentioned by coincidence.
# ===========================================================================
printf '\nnamed repo argument:\n'

# Give repo2 a HELD branch of its own, so "excluded" has something to exclude.
git -C "$REPO2" checkout -q -b spira/tst-r2held
git -C "$REPO2" commit --allow-empty -m "other repo work"
git -C "$REPO2" checkout -q "$BASE_BR2"

# Restore fixture repo-map with both repos.
printf 'fixture | %s | hold | | |\nfixture2 | %s | hold | | |\n' \
    "$REPO" "$REPO2" > "$SPIRA_REPO_MAP"

tbl_both="$(bash "$HERE/held.sh" 2>&1)"
want "with no repo argument, fixture2's branch is shown" "spira/tst-r2held" "$tbl_both"

tbl_named="$(bash "$HERE/held.sh" fixture 2>&1)"
want   "named arg shows fixture"           "fixture"         "$tbl_named"
want   "named arg shows HELD verdict"      "HELD"            "$tbl_named"
nowant "named arg excludes the other repo's name"   "fixture2"          "$tbl_named"
nowant "named arg excludes the other repo's branch" "spira/tst-r2held"  "$tbl_named"

# ===========================================================================
# SECTION 5 — bd unreadable: UNKNOWN, never ORPHAN, exit non-zero (sp-f84wv).
# ===========================================================================
printf '\nbd unreadable -> UNKNOWN, never ORPHAN, exit non-zero:\n'

git -C "$REPO" checkout -q -b spira/tst-unknown
git -C "$REPO" commit --allow-empty -m "unknown work"
git -C "$REPO" checkout -q "$BASE_BR"

# Positive control: with the stub answering normally, this same never-seeded id is
# legitimately ORPHAN — proving the matcher can tell "gone" from "unreadable" apart
# (law-absence-needs-a-positive-control).
baseline_line="$(bash "$HERE/held.sh" fixture 2>&1 | grep 'tst-unknown')"
want "tst-unknown is ORPHAN when bd works and the bead is absent" "ORPHAN" "$baseline_line"

unk_tbl="$(SPIRA_TEST_BD_FAIL_UNKNOWN=1 bash "$HERE/held.sh" fixture 2>&1)"; unk_rc=$?
if [ "$unk_rc" -ne 0 ]; then ok "held.sh exits non-zero when bd is unreadable"
else bad "held.sh exits non-zero when bd is unreadable: got exit 0"; fi

unk_line="$(printf '%s\n' "$unk_tbl" | grep 'tst-unknown')"
want   "tst-unknown row shows UNKNOWN when bd fails"          "UNKNOWN" "$unk_line"
nowant "tst-unknown row is never reported ORPHAN when bd fails" "ORPHAN" "$unk_line"
want "table names the unknown count" "unknown — bd could not be read" "$unk_tbl"

tl_summary
