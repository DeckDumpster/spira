#!/usr/bin/env bash
#
# test-land-commit-contract.sh — the "spira: land <id>" commit message, written by
# batch.sh/verdict.sh/landing.sh, read by lib.sh's landed()/landed_sha() and by
# gh-issue-backfill.sh's own ancestry search.
#
# gap G4 (docs/test-plan/landing-merge-queue.md section 6): the writer is one literal
# string, produced at three call sites, but it has two independent readers — lib.sh's
# landed()/landed_sha(), and gh-issue-backfill.sh's own git log --grep search, which does
# not call landed() at all (it takes the first ancestry match by id, no subject-shape
# check). Nothing before this suite built ONE fixture and asked both readers about it;
# CLOSED != LANDED (law-closed-is-not-landed) depends on them agreeing.
#
# tier: T2
# covers: spira/lib.sh spira/gh-issue-backfill.sh spira/batch.sh spira/verdict.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-land-commit-contract
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landcommit || skip "testdb not available"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m "initial"

echo "test-land-commit-contract.sh"

# ============================================================================
echo
echo "POSITIVE CONTROL — a bead with no commit at all is not landed by either reader:"
# ============================================================================
SH="$TMP/spira"; mkdir -p "$SH"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
printf 'fixture | %s\n' "$REPO" > "$SH/repo-map"
export SPIRA_HOME="$SH" SPIRA_REPO_MAP="$SH/repo-map" SPIRA_HOME_REPO=fixture SPIRA_REPO="$REPO"
. "$SH/lib.sh"

landed sp-nocommit "$REPO" 2>/dev/null
is "landed(): no commit -> not landed" "1" "$?"

# ============================================================================
echo
echo "THE WRITER FORM — a real 'spira: land <id>' commit, exactly what batch.sh/verdict.sh/landing.sh write:"
# ============================================================================
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-fix"
FIX_SHA="$(git -C "$REPO" rev-parse HEAD)"

landed sp-fix "$REPO" 2>/dev/null
is "landed(): the writer form is recognised" "0" "$?"

LIB_SHA="$(landed_sha sp-fix "$REPO" 2>/dev/null)"
is "landed_sha(): returns the writer commit's own sha" "$FIX_SHA" "$LIB_SHA"

# ============================================================================
echo
echo "THE OTHER READER — gh-issue-backfill.sh's own ancestry search agrees with landed_sha():"
# gh-issue-backfill.sh does not call landed()/landed_sha(); it runs its own git log
# --grep="\$id" ancestry search (no subject-shape filter) and trusts the first hit. On
# this fixture — one candidate commit, the writer form itself — the two readers must
# name the same sha, or CHECK 5's ancestry-based verdict and the backfill's github-issue
# close are deciding "landed" from different evidence.
# ============================================================================
GHLOG="$TMP/gh.log"
mkdir -p "$TMP/bin"
cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view "*) printf '{"state":"OPEN"}\n' ;;
esac
exit 0
GHSTUB
chmod +x "$TMP/bin/gh"
export GHLOG SPIRA_GH="$TMP/bin/gh"

RUN="$TMP/run"; mkdir -p "$RUN/gh-closed" "$RUN/landstate"

testdb_seed <<JSONL
{"id":"sp-fix","title":"the fix","status":"closed","issue_type":"bug","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#7","updated_at":"2026-09-05T00:00:00Z"}
JSONL

: > "$GHLOG"
out="$(SPIRA_GH="$TMP/bin/gh" GHLOG="$GHLOG" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" \
     SPIRA_RUN="$RUN" SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixture SPIRA_REPO="$REPO" \
     SPIRA_REPO_MAP="$SH/repo-map" bash "$SH/gh-issue-backfill.sh" --dry-run 2>&1)"

BACKFILL_SHA_PREFIX="$(printf '%s\n' "$out" | grep -oE 'as [0-9a-f]{8}' | awk '{print $2}')"
if [ -n "$BACKFILL_SHA_PREFIX" ]; then
    ok "gh-issue-backfill.sh (dry-run) reports a landed sha for sp-fix"
else
    bad "gh-issue-backfill.sh (dry-run) reports a landed sha for sp-fix" "got: $out"
fi
is "the two readers name the same commit" "${LIB_SHA:0:8}" "${BACKFILL_SHA_PREFIX:-}"

tl_summary
