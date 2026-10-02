#!/usr/bin/env bash
#
# test-land-commit-contract.sh — the "spira: land <id>" commit message, written by
# the batcher/queue verdict/landing.sh, read by lib.sh's landed()/landed_sha() and by
# gh-issue-backfill.sh's own ancestry search.
#
# gap G4 (docs/test-plan/landing-merge-queue.md section 6): the writer is one literal
# string, produced at three call sites, but it has two independent readers — lib.sh's
# landed()/landed_sha(), and `gh-intake backfill`'s own git log --grep search (ported
# from gh-issue-backfill.sh, sp-j3fim, "wave 4.31"), which does not call landed() at all
# (it takes the first ancestry match by id, no subject-shape check). Nothing before this
# suite built ONE fixture and asked both readers about it; CLOSED != LANDED
# (law-closed-is-not-landed) depends on them agreeing.
#
# tier: T2
# covers: spira/lib.sh gh-intake/src/* queue/src/* landing-pass/src/*
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
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
printf 'fixture | %s\n' "$REPO" > "$SH/repo-map"
export SPIRA_HOME="$SH" SPIRA_REPO_MAP="$SH/repo-map" SPIRA_HOME_REPO=fixture SPIRA_REPO="$REPO"
. "$SH/lib.sh"

landing-pass landed sp-nocommit "$REPO" 2>/dev/null
is "landed(): no commit -> not landed" "1" "$?"

# ============================================================================
echo
echo "THE WRITER FORM — a real 'spira: land <id>' commit, exactly what the batcher/queue verdict/landing.sh write:"
# ============================================================================
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-fix"
FIX_SHA="$(git -C "$REPO" rev-parse HEAD)"

landing-pass landed sp-fix "$REPO" 2>/dev/null
is "landed(): the writer form is recognised" "0" "$?"

LIB_SHA="$(landing-pass landed sp-fix "$REPO" 2>/dev/null)"
is "landed_sha(): returns the writer commit's own sha" "$FIX_SHA" "$LIB_SHA"

# ============================================================================
echo
echo "THE WIDENED FORM — land_subject() appends the bead's own title, and landed()/landed_sha() still anchor on the id:"
# ============================================================================
testdb_seed <<JSONL
{"id":"sp-titled","title":"a fix with a title","status":"closed","issue_type":"bug","labels":["spira","plan"],"updated_at":"2026-09-05T00:00:00Z"}
JSONL

SUBJ="$(land_subject sp-titled)"
is "land_subject(): appends the bead's own title" "spira: land sp-titled — a fix with a title" "$SUBJ"

git -C "$REPO" commit -q --allow-empty -m "$SUBJ"
TITLED_SHA="$(git -C "$REPO" rev-parse HEAD)"

landing-pass landed sp-titled "$REPO" 2>/dev/null
is "landed(): the titled writer form is recognised" "0" "$?"

TITLED_LIB_SHA="$(landing-pass landed sp-titled "$REPO" 2>/dev/null)"
is "landed_sha(): returns the titled writer commit's own sha" "$TITLED_SHA" "$TITLED_LIB_SHA"

# A longer id that merely starts with this one's id, even in the titled form, must not
# false-match — the case pattern anchors on the id followed by a space, never a bare prefix.
# sp-titledx's own titled commit lands AFTER sp-titled's, so a prefix-matching reader would
# report sp-titled's tip as this newer commit; the anchored one must not.
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-titledx — a different, unrelated bead"
is "landed_sha(): a longer id's titled commit does not shadow this id's own" \
    "$TITLED_SHA" "$(landing-pass landed sp-titled "$REPO" 2>/dev/null)"

# ============================================================================
echo
echo "THE OTHER READER — gh-intake backfill's own ancestry search agrees with landed_sha():"
# gh-intake backfill (ported from gh-issue-backfill.sh, sp-j3fim "wave 4.31") does not
# call landed()/landed_sha(); it runs its own git log --grep="\$id" ancestry search (no
# subject-shape filter) and trusts the first hit. On this fixture — one candidate commit,
# the writer form itself — the two readers must name the same sha, or CHECK 5's
# ancestry-based verdict and the backfill's github-issue close are deciding "landed" from
# different evidence.
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
     SPIRA_REPO_MAP="$SH/repo-map" gh-intake backfill --dry-run 2>&1)"

BACKFILL_SHA_PREFIX="$(printf '%s\n' "$out" | grep -oE 'as [0-9a-f]{8}' | awk '{print $2}')"
if [ -n "$BACKFILL_SHA_PREFIX" ]; then
    ok "gh-intake backfill (dry-run) reports a landed sha for sp-fix"
else
    bad "gh-intake backfill (dry-run) reports a landed sha for sp-fix" "got: $out"
fi
is "the two readers name the same commit" "${LIB_SHA:0:8}" "${BACKFILL_SHA_PREFIX:-}"

tl_summary
