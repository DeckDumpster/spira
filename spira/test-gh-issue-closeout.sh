#!/usr/bin/env bash
#
# test-gh-issue-closeout.sh — gh_issue_closeout comments and closes the linked
# GitHub issue when a bead's commit is on the base branch; a closed-but-unlanded
# bead is not auto-closed.
#
# POSITIVE CONTROL: a fixture bead whose landstate is LANDED gets a gh comment
# and close. With the current code (no gh_issue_closeout) both assertions fail.
#
# NEGATIVE CONTROL: a bead closed without a LANDED landstate entry does NOT get
# its issue auto-closed. That property is enforced by the landing pass only calling
# gh_issue_closeout on LANDED beads; the backfill script applies the same filter.
#
# covers: spira/landing.sh spira/lib.sh spira/gh-issue-backfill.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

printf 'test-gh-issue-closeout.sh\n\n'

. "$HERE/testdb.sh"
testdb_require test-gh-issue-closeout
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up gh-closeout || {
    printf 'SKIP test-gh-issue-closeout: server testdb not available\n' >&2
    exit 77
}

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# --- git fixture ---
REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m "initial"
git -C "$REPO" commit -q --allow-empty -m "spira: land fixture-landed"
LANDED_SHA="$(git -C "$REPO" rev-parse HEAD)"

# --- gh mock ---
GHLOG="$TMP/gh.log"
mkdir -p "$TMP/bin"
cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
# Record every call. Return OPEN for view; succeed silently for comment/close.
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view "*) printf '{"state":"OPEN"}\n' ;;
esac
exit 0
GHSTUB
chmod +x "$TMP/bin/gh"
export GHLOG

# --- spira environment ---
RUN="$TMP/run"
mkdir -p "$RUN/gh-closed" "$RUN/landstate"
export SPIRA_RUN="$RUN"
export SPIRA_GH="$TMP/bin/gh"

SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE"/*.sh "$SH/"

# Minimal repo-map for backfill: one repo named "fixture" at the git fixture path.
printf 'fixture | %s\n' "$REPO" > "$SH/repo-map"

# --- bd fixture: seed beads with explicit IDs ---
# SPIRA_DB and SPIRA_BD are set by testdb_up above.
testdb_seed <<JSONL
{"id":"sp-tgh1","title":"Fix issue 1","status":"open","issue_type":"bug","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#1","updated_at":"2026-09-05T00:00:00Z"}
{"id":"sp-tgh2","title":"Fix issue 2 not landed","status":"open","issue_type":"bug","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#2","updated_at":"2026-09-05T00:00:00Z"}
{"id":"sp-tgh3","title":"No external ref","status":"open","issue_type":"bug","labels":["spira","plan"],"updated_at":"2026-09-05T00:00:00Z"}
JSONL
BEAD1=sp-tgh1
BEAD2=sp-tgh2
BEAD3=sp-tgh3
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD1" --reason-file - <<< "done" 2>/dev/null || true
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD2" --reason-file - <<< "done" 2>/dev/null || true
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD3" --reason-file - <<< "done" 2>/dev/null || true

# Set landstate: BEAD1 is LANDED; BEAD2 has no landstate
printf 'LANDED %s %s push\n' "$LANDED_SHA" "$(date +%s)" > "$RUN/landstate/$BEAD1"

# Source lib.sh for the functions under test.
export SPIRA_HOME="$SH"
. "$SH/lib.sh"

printf '1. gh_issue_closeout is defined:\n'
if declare -f gh_issue_closeout >/dev/null 2>&1; then
    ok "gh_issue_closeout is defined"
else
    bad "gh_issue_closeout is defined" "function not found in lib.sh"
fi

printf '\n2. a landed bead closes its github issue:\n'
: > "$GHLOG"
gh_issue_closeout "$BEAD1" "$LANDED_SHA" "$REPO" 2>/dev/null
if grep -q "issue comment" "$GHLOG" 2>/dev/null; then
    ok "gh comment was posted"
else
    bad "gh comment was posted" "no 'issue comment' call — GHLOG: $(cat "$GHLOG" 2>/dev/null | head -3)"
fi
if grep -q "issue close" "$GHLOG" 2>/dev/null; then
    ok "gh issue was closed"
else
    bad "gh issue was closed" "no 'issue close' call"
fi

# Comment must cite the short sha and a commit link.
COMMENT_LINE="$(grep "issue comment" "$GHLOG" | tail -1)"
SHORT_SHA="$(git -C "$REPO" rev-parse --short "$LANDED_SHA")"
if [[ "$COMMENT_LINE" == *"$SHORT_SHA"* ]]; then
    ok "comment cites the short sha"
else
    bad "comment cites the short sha" "sha $SHORT_SHA not in: $COMMENT_LINE"
fi
if [[ "$COMMENT_LINE" == *"https://github.com/fixture/testrepo/commit/"* ]]; then
    ok "comment contains commit link"
else
    bad "comment contains commit link" "no commit link in: $COMMENT_LINE"
fi

# Close marker must exist.
if [ -e "$RUN/gh-closed/$BEAD1" ]; then
    ok "close marker written"
else
    bad "close marker written" "no marker at $RUN/gh-closed/$BEAD1"
fi

printf '\n3. a second call is idempotent:\n'
: > "$GHLOG"
gh_issue_closeout "$BEAD1" "$LANDED_SHA" "$REPO" 2>/dev/null
if [ -s "$GHLOG" ]; then
    bad "no gh call on second invocation" "got: $(cat "$GHLOG")"
else
    ok "no gh call on second invocation"
fi

printf '\n4. a closed-but-unlanded bead is not auto-closed:\n'
: > "$GHLOG"
# The backfill script enforces this: it only calls closeout for LANDED beads.
out="$(SPIRA_GH="$TMP/bin/gh" GHLOG="$GHLOG" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" \
     SPIRA_RUN="$RUN" SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixture SPIRA_REPO="$REPO" \
     SPIRA_REPO_MAP="$SH/repo-map" bash "$SH/gh-issue-backfill.sh" 2>&1)"
if grep -q "issue close" "$GHLOG" 2>/dev/null; then
    # Fail if bead2 was closed
    if grep -q "fixture/testrepo#2" "$GHLOG"; then
        bad "unlanded bead issue not closed" "bead2 issue was closed by backfill"
    else
        ok "only landed bead issues are closed"
    fi
else
    # No closes at all — bead2 has no landstate, bead1's marker already exists
    ok "unlanded bead issue not auto-closed"
fi
# bead2 should NOT have a close marker
if [ -e "$RUN/gh-closed/$BEAD2" ]; then
    bad "unlanded bead has no close marker" "marker exists at $RUN/gh-closed/$BEAD2"
else
    ok "no close marker for unlanded bead"
fi

printf '\n5. a bead with no github ref is a no-op:\n'
: > "$GHLOG"
gh_issue_closeout "$BEAD3" "$LANDED_SHA" "$REPO" 2>/dev/null
if [ -s "$GHLOG" ]; then
    bad "no gh call for non-github bead" "got: $(cat "$GHLOG")"
else
    ok "no gh call for non-github bead"
fi

printf '\n6. the backfill script finds landed beads and closes their issues:\n'
# Remove bead1's close marker so backfill has something to do.
rm -f "$RUN/gh-closed/$BEAD1"
: > "$GHLOG"
out="$(SPIRA_GH="$TMP/bin/gh" GHLOG="$GHLOG" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" \
     SPIRA_RUN="$RUN" SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixture SPIRA_REPO="$REPO" \
     SPIRA_REPO_MAP="$SH/repo-map" bash "$SH/gh-issue-backfill.sh" 2>&1)"
if grep -q "issue comment" "$GHLOG" 2>/dev/null; then
    ok "backfill posted a comment"
else
    bad "backfill posted a comment" "no comment in gh log: $out"
fi
if grep -q "issue close" "$GHLOG" 2>/dev/null; then
    ok "backfill closed the issue"
else
    bad "backfill closed the issue" "no close in gh log"
fi

printf '\n7. the backfill --dry-run makes no gh calls:\n'
rm -f "$RUN/gh-closed/$BEAD1"
: > "$GHLOG"
out="$(SPIRA_GH="$TMP/bin/gh" GHLOG="$GHLOG" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" \
     SPIRA_RUN="$RUN" SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixture SPIRA_REPO="$REPO" \
     SPIRA_REPO_MAP="$SH/repo-map" bash "$SH/gh-issue-backfill.sh" --dry-run 2>&1)"
if grep -q "issue" "$GHLOG" 2>/dev/null; then
    bad "dry-run makes no gh calls" "got: $(cat "$GHLOG")"
else
    ok "dry-run makes no gh calls"
fi
if [[ "$out" == *"would close"* ]]; then
    ok "dry-run reports what would be closed"
else
    bad "dry-run reports what would be closed" "got: $out"
fi

printf '\n  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
