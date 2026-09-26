#!/usr/bin/env bash
#
# test-citations.sh — every test carries its citation, and citations.sh reports them.
#
#   ./test-citations.sh
#
# WHAT THIS GUARDS. A test that has never gone red since it was written is either protecting
# something expensive or is dead weight, and nothing about the test says which. citations.sh
# is the mechanism: it reads `# defect:` declarations from suites, queries the database, and
# reports three states — uncited, resolved, and unresolved. The three states must be DISTINCT:
# a suite with no declaration must not read identically to one whose bead is not found, and
# neither must read like one whose bead is found — each calls for a different response, so
# collapsing them hides either a research gap or a mistype (law-absence-needs-a-positive-control).
#
# A REAL bd ON A THROWAWAY DATABASE (law-prefer-the-real-dependency). The distinction between
# "resolved" and "unresolved" is the database query, and a stub that returns nothing for every
# missing bead would pass the uncited test while silently reporting resolved beads as unresolved.
#
# THE ENVIRONMENT IS EXPLICIT AND MINIMAL. SPIRA_CONF is pointed at a nonexistent file;
# CITATIONS_DIR overrides the directory citations.sh reads, so the suite does not assert
# against the real tree — which changes whenever any test is added or cited — and does not
# write into a real Spira database (law-gates-run-in-a-clean-environment).
#
# defect: sp-gsmx.7
# tier: T2
# covers: spira/citations.sh spira/test-*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-citations.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-citations

TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up citations || { printf 'test-citations: could not build a fixture database\n'; exit 1; }

# A bead the suite can cite and verify resolves. Status "closed" is the expected state for
# a defect a test exists for — the regression was fixed, the test guards the fix.
testdb_seed <<'JSONL'
{"id":"sp-fixture-cite","title":"a defect whose test can name it","status":"closed","issue_type":"task","labels":["test"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL

export SPIRA_CONF="$TMP/no-such-conf"
CITATIONS="$HERE/citations.sh"

# Build a fixture directory with three suites, one per citation state.
# These are not the real spira/test-*.sh files — the property under test is the
# convention and the report, not the current state of any specific suite.
FIXTURE_DIR="$TMP/suites"
mkdir -p "$FIXTURE_DIR"

# Suite 1: citation that resolves to a real closed bead.
cat > "$FIXTURE_DIR/test-cited.sh" <<'EOF'
#!/usr/bin/env bash
# defect: sp-fixture-cite
# covers: nothing
set -uo pipefail
echo "ok"
EOF

# Suite 2: citation whose bead id is not in the database — a plausible id, not in the db.
cat > "$FIXTURE_DIR/test-unresolved.sh" <<'EOF'
#!/usr/bin/env bash
# defect: sp-no-such-bead-zzzz
# covers: nothing
set -uo pipefail
echo "ok"
EOF

# Suite 3: no citation — retirement is undecidable; the report surfaces it.
cat > "$FIXTURE_DIR/test-uncited.sh" <<'EOF'
#!/usr/bin/env bash
# covers: nothing
set -uo pipefail
echo "ok"
EOF

echo
echo "=== list: the three states are distinct and correctly labelled ==="
# THE POSITIVE CONTROL IS FIRST: before believing that uncited reads as uncited, confirm
# that the same report shows the cited suite as resolved. If the report collapsed all three
# states into one word we would see it here, because resolved would also read as uncited.
out="$(CITATIONS_DIR="$FIXTURE_DIR" bash "$CITATIONS" list 2>&1)"

want "cited suite appears in the report"        "test-cited.sh"           "$out"
want "cited suite carries its bead id"          "sp-fixture-cite"         "$out"
want "cited bead resolves"                      "resolved ("              "$out"
want "resolved shows the bead status"           "closed"                  "$out"

want "uncited suite appears in the report"      "test-uncited.sh"         "$out"
want "uncited suite is labelled uncited"        "uncited"                 "$out"

want "unresolved suite appears in the report"   "test-unresolved.sh"      "$out"
want "unresolved suite carries its bead id"     "sp-no-such-bead-zzzz"   "$out"
want "unresolved suite is labelled unresolved"  "unresolved"              "$out"

# DISTINCTNESS: if any pair collapsed, "resolved" would appear on an uncited or unresolved
# line, or "unresolved" on an uncited line, or "uncited" on a cited or unresolved line.
nowant "uncited line does not say resolved"     "resolved"    "$(printf '%s\n' "$out" | grep test-uncited)"
nowant "uncited line does not say unresolved"   "unresolved"  "$(printf '%s\n' "$out" | grep test-uncited)"
nowant "unresolved line does not say resolved"  "resolved ("  "$(printf '%s\n' "$out" | grep test-unresolved)"
nowant "uncited line shows no bead id"          "sp-"         "$(printf '%s\n' "$out" | grep test-uncited)"

echo
echo "=== resolve: resolves a cited suite to its defect ==="
out="$(CITATIONS_DIR="$FIXTURE_DIR" bash "$CITATIONS" resolve test-cited.sh 2>&1)"
want "resolve prints the bead id"      "sp-fixture-cite"  "$out"
want "resolve prints the bead status"  "closed"           "$out"
want "resolve prints the bead title"   "a defect"         "$out"

echo
echo "=== resolve: reports uncited for a suite with no declaration ==="
out="$(CITATIONS_DIR="$FIXTURE_DIR" bash "$CITATIONS" resolve test-uncited.sh 2>&1)"
want "resolve says uncited" "uncited" "$out"

echo
echo "=== resolve: distinguishes unresolved from uncited ==="
out="$(CITATIONS_DIR="$FIXTURE_DIR" bash "$CITATIONS" resolve test-unresolved.sh 2>&1)" || true
# The citation is named (so the caller sees what was tried), and "does not resolve" is
# distinct from "uncited" (so the caller knows it is a mistype or wrong db, not a gap).
want "resolve names the citation"            "sp-no-such-bead-zzzz"  "$out"
want "resolve says does not resolve"         "does not resolve"       "$out"
nowant "unresolved resolve does not say uncited" "uncited"            "$out"

echo
tl_summary
