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
# tier: T2
# covers: spira/landing.sh spira/lib.sh spira/gh-issue-backfill.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

printf 'test-gh-issue-closeout.sh\n\n'

. "$HERE/testdb.sh"
testdb_require test-gh-issue-closeout
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up gh-closeout || {
    printf 'SKIP test-gh-issue-closeout: testdb not available\n' >&2
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
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

# Minimal repo-map for backfill: one repo named "fixture" at the git fixture path.
printf 'fixture | %s\n' "$REPO" > "$SH/repo-map"

# --- bd fixture: seed beads with explicit IDs ---
# SPIRA_DB and SPIRA_BD are set by testdb_up above.
testdb_seed <<JSONL
{"id":"sp-tgh1","title":"Fix issue 1","status":"open","issue_type":"bug","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#1","updated_at":"2026-09-05T00:00:00Z"}
{"id":"sp-tgh2","title":"Fix issue 2 not landed","status":"open","issue_type":"bug","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#2","updated_at":"2026-09-05T00:00:00Z"}
{"id":"sp-tgh3","title":"No external ref","status":"open","issue_type":"bug","labels":["spira","plan"],"updated_at":"2026-09-05T00:00:00Z"}
{"id":"sp-tgh4","title":"Fix issue 4 stale-landstate","status":"open","issue_type":"bug","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#4","updated_at":"2026-09-05T00:00:00Z"}
JSONL
BEAD1=sp-tgh1
BEAD2=sp-tgh2
BEAD3=sp-tgh3
BEAD4=sp-tgh4
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD1" --reason-file - <<< "done" 2>/dev/null || true
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD2" --reason-file - <<< "done" 2>/dev/null || true
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD3" --reason-file - <<< "done" 2>/dev/null || true
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD4" --reason-file - <<< "done" 2>/dev/null || true

# Set landstate: BEAD1 is LANDED; BEAD2 and BEAD4 have no landstate yet
printf 'LANDED %s %s push\n' "$LANDED_SHA" "$(date +%s)" > "$RUN/landstate/$BEAD1"

# Source lib.sh for the functions under test. bead_repo/repo_root (used by
# _gh_unlanded_scan's landed() check) need the same repo mapping the backfill
# subprocess calls above are given inline, but in-process this time.
export SPIRA_HOME="$SH"
# Explicit, or a container with a real installed harness leaves SPIRA_REPO_MAP already
# set and conf.sh's "only resolve when unset" guard never looks at $SH/repo-map at all
# (law-gates-run-in-a-clean-environment) — tests 6-8 dodge this by overriding it only on
# the gh-issue-backfill.sh subshell; direct calls to _gh_unlanded_scan need it too.
export SPIRA_REPO_MAP="$SH/repo-map"
export SPIRA_HOME_REPO=fixture
export SPIRA_REPO="$REPO"
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
if grep -q "https://github.com/fixture/testrepo/commit/" "$GHLOG" 2>/dev/null; then
    ok "comment contains commit link"
else
    bad "comment contains commit link" "no commit link in gh log"
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

printf '\n8. stale GATED landstate: backfill uses ancestry, not landstate state:\n'
# POSITIVE CONTROL: a bead whose landstate is stale GATED but whose commit IS on
# the land ref gets its GitHub issue closed by the backfill. Against the old code
# (which trusted landstate state==LANDED) this bead would be skipped silently.
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-tgh4 stale-landstate-test"
# Stale landstate: state says GATED, sha is a non-existent object.
printf 'GATED 0000000000000000000000000000000000000000 0 NO_VERDICT:tree-unidentified\n' \
    > "$RUN/landstate/$BEAD4"
: > "$GHLOG"
out="$(SPIRA_GH="$TMP/bin/gh" GHLOG="$GHLOG" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" \
     SPIRA_RUN="$RUN" SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixture SPIRA_REPO="$REPO" \
     SPIRA_REPO_MAP="$SH/repo-map" bash "$SH/gh-issue-backfill.sh" 2>&1)"
if grep -q "issue close 4" "$GHLOG" 2>/dev/null; then
    ok "stale-GATED bead closed by ancestry check"
else
    bad "stale-GATED bead closed by ancestry check" \
        "issue 4 not closed — out: $out, ghlog: $(cat "$GHLOG" 2>/dev/null)"
fi
if [ -e "$RUN/gh-closed/$BEAD4" ]; then
    ok "close marker written for stale-landstate bead"
else
    bad "close marker written for stale-landstate bead" "no marker at $RUN/gh-closed/$BEAD4"
fi

printf '\n9. _gh_unlanded_scan: CERTIFIED landstate logs waiting, sends no ask:\n'
# Pre-mark existing unlanded test beads so they don't fire in the scan tests.
: > "$RUN/gh-closed/sp-tgh2"
# Seed a closed bead with CERTIFIED landstate.
testdb_seed <<JSONL
{"id":"sp-scan1","title":"Scan CERTIFIED bead","status":"closed","issue_type":"task","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#91","updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z"}
JSONL
printf 'CERTIFIED abc1234 %s\n' "$(date +%s)" > "$RUN/landstate/sp-scan1"
export SPIRA_ASK_LABEL=needs-operator
export SPIRA_MAIL_KINDS="$HERE/mail/kinds"
export SPIRA_MAIL="$RUN/mail"
MAIL_CALLS="$TMP/mail.calls"
: > "$MAIL_CALLS"
# Stub mail.sh: any call writes to MAIL_CALLS (a call here is a test failure).
cat > "$SH/mail.sh" <<MAILSTUB
#!/usr/bin/env bash
echo called >> $MAIL_CALLS
exit 0
MAILSTUB
chmod +x "$SH/mail.sh"
: > "$GHLOG"
scan_out="$(_gh_unlanded_scan 2>&1)"
if [[ "$scan_out" == *"waiting on landing"* ]]; then
    ok "CERTIFIED: 'waiting on landing' logged"
else
    bad "CERTIFIED: 'waiting on landing' logged" "got: $scan_out"
fi
if [ -s "$MAIL_CALLS" ]; then
    bad "CERTIFIED: no ask sent" "mail.sh was called: $(cat "$MAIL_CALLS")"
else
    ok "CERTIFIED: no ask sent"
fi

printf '\n10. _gh_unlanded_scan: mail.sh refuse logs ask refused with reason:\n'
testdb_seed <<JSONL
{"id":"sp-scan2","title":"Scan unlanded bead","status":"closed","issue_type":"task","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#92","updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z"}
JSONL
# Restore gh stub to return OPEN for issue view.
: > "$GHLOG"
cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view "*) printf 'OPEN\n' ;;
esac
exit 0
GHSTUB
chmod +x "$TMP/bin/gh"
# Stub mail.sh to refuse with a reason on stderr.
cat > "$SH/mail.sh" <<'MAILSTUB'
#!/usr/bin/env bash
printf 'mail: repeat refused — stub\n' >&2
exit 1
MAILSTUB
chmod +x "$SH/mail.sh"
scan_out="$(_gh_unlanded_scan 2>&1)"
if [[ "$scan_out" == *"ask refused"* ]]; then
    ok "refused: 'ask refused' logged"
else
    bad "refused: 'ask refused' logged" "got: $scan_out"
fi
if [[ "$scan_out" == *"repeat refused"* ]]; then
    ok "refused: reason from mail.sh in log"
else
    bad "refused: reason from mail.sh in log" "got: $scan_out"
fi
if [[ "$scan_out" == *"asked operator"* ]]; then
    bad "refused: 'asked operator' not logged on refusal" "got: $scan_out"
else
    ok "refused: 'asked operator' not logged on refusal"
fi

printf '\n11. mail.sh success logs asked operator; second pass silent:\n'
# Stub mail.sh to succeed.
cat > "$SH/mail.sh" <<'MAILSTUB'
#!/usr/bin/env bash
exit 0
MAILSTUB
chmod +x "$SH/mail.sh"
scan_out="$(_gh_unlanded_scan 2>&1)"
if [[ "$scan_out" == *"asked operator"* ]]; then
    ok "success: 'asked operator' logged"
else
    bad "success: 'asked operator' logged" "got: $scan_out"
fi
# Simulate the tracking ask bead now existing (as mail.sh with --bead would create).
_ask_subj="Close GitHub issue github:fixture/testrepo#92 for bead sp-scan2"
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" create "$_ask_subj" \
    -l "needs-operator,overseer" --type decision --silent >/dev/null 2>&1 || true
# Second pass must produce no output (dedupe via ask_already_open finds the bead).
scan_out2="$(_gh_unlanded_scan 2>&1)"
if [ -z "$scan_out2" ]; then
    ok "second pass: silent (dedupe held)"
else
    bad "second pass: silent (dedupe held)" "got: $scan_out2"
fi

printf '\n12. gh_issue_ask_unlanded: issue already closed on forge writes marker, no mail:\n'
testdb_seed <<JSONL
{"id":"sp-scan3","title":"Forge-closed bead","status":"closed","issue_type":"task","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#93","updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z"}
JSONL
# Stub gh to return CLOSED for issue 93's view only. _gh_unlanded_scan rescans the
# WHOLE backlog every call, sp-scan2's ask included, so a stub that answered CLOSED
# for every issue view — not just #93's — would silently mark every other pending
# bead gh-closed too, sp-scan2 among them, in this one pass.
: > "$GHLOG"
cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view 93 "*) printf 'CLOSED\n' ;;
    *" issue view "*)    printf '{"state":"OPEN"}\n' ;;
esac
exit 0
GHSTUB
chmod +x "$TMP/bin/gh"
MAIL_CALLS2="$TMP/mail.calls2"
: > "$MAIL_CALLS2"
cat > "$SH/mail.sh" <<MAILSTUB
#!/usr/bin/env bash
echo called >> $MAIL_CALLS2
exit 0
MAILSTUB
chmod +x "$SH/mail.sh"
scan_out="$(_gh_unlanded_scan 2>&1)"
if [ -e "$RUN/gh-closed/sp-scan3" ]; then
    ok "forge-closed: gh-closed marker written"
else
    bad "forge-closed: gh-closed marker written" "marker not at $RUN/gh-closed/sp-scan3"
fi
if [[ "$scan_out" == *"already closed on forge"* ]]; then
    ok "forge-closed: log line about being closed"
else
    bad "forge-closed: log line about being closed" "got: $scan_out"
fi
if [ -s "$MAIL_CALLS2" ]; then
    bad "forge-closed: no mail sent" "mail.sh was called: $(cat "$MAIL_CALLS2")"
else
    ok "forge-closed: no mail sent"
fi

printf '\n13. _gh_unlanded_scan: land commit on base, no landstate — closed by ancestry, no ask:\n'
# POSITIVE CONTROL for law-landed-is-content: a bead whose commit IS on the land ref but
# has NO landstate file at all (the exact shape of the 20 false asks this bead fixes).
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-scan4"
testdb_seed <<JSONL
{"id":"sp-scan4","title":"Scan landed-by-commit bead","status":"closed","issue_type":"task","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#94","closed_at":"2026-09-05T00:00:00Z"}
JSONL
rm -f "$RUN/landstate/sp-scan4"
: > "$GHLOG"
cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view "*) printf '{"state":"OPEN"}\n' ;;
esac
exit 0
GHSTUB
chmod +x "$TMP/bin/gh"
MAIL_CALLS3="$TMP/mail.calls3"
: > "$MAIL_CALLS3"
cat > "$SH/mail.sh" <<MAILSTUB
#!/usr/bin/env bash
echo called >> $MAIL_CALLS3
exit 0
MAILSTUB
chmod +x "$SH/mail.sh"
_gh_unlanded_scan >/dev/null 2>&1
if grep -q "issue comment 94" "$GHLOG" 2>/dev/null && grep -q "issue close 94" "$GHLOG" 2>/dev/null; then
    ok "landed-by-commit: issue closed via ancestry, not landstate"
else
    bad "landed-by-commit: issue closed via ancestry, not landstate" "GHLOG: $(cat "$GHLOG" 2>/dev/null)"
fi
if [ -e "$RUN/gh-closed/sp-scan4" ]; then
    ok "landed-by-commit: close marker written"
else
    bad "landed-by-commit: close marker written" "no marker at $RUN/gh-closed/sp-scan4"
fi
if [ -s "$MAIL_CALLS3" ]; then
    bad "landed-by-commit: no ask sent" "mail.sh was called: $(cat "$MAIL_CALLS3")"
else
    ok "landed-by-commit: no ask sent"
fi

printf '\n14. _gh_unlanded_scan: a bead with no commit anywhere still produces exactly one ask (positive control):\n'
# Proves test 13's silence means something: the same scan, on a bead that really
# is not landed, still asks (law-absence-needs-a-positive-control). closed_at must be
# explicit and past the grace period: bd import stamps an absent closed_at with the
# import time, which would put this bead inside SPIRA_GH_ASK_GRACE_SECS and suppress
# the very ask this test exists to prove.
testdb_seed <<JSONL
{"id":"sp-scan5","title":"Scan truly unlanded bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:fixture"],"external_ref":"github:fixture/testrepo#95","updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z"}
JSONL
: > "$GHLOG"
cat > "$SH/mail.sh" <<'MAILSTUB'
#!/usr/bin/env bash
exit 0
MAILSTUB
chmod +x "$SH/mail.sh"
scan_out="$(_gh_unlanded_scan 2>&1)"
want "positive control: ask sent for the unlanded bead" "asked operator about github:fixture/testrepo#95" "$scan_out"
# Simulate the tracking bead mail.sh would have created, so later scans in this file
# dedupe sp-scan5 through ask_already_open instead of re-asking on every pass.
ASK_ID="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" create \
    "Close GitHub issue github:fixture/testrepo#95 for bead sp-scan5" \
    -l "needs-operator,overseer" --type decision --silent 2>/dev/null)"

printf '\n15. an existing open ask is resolved once its issue is found CLOSED:\n'
: > "$GHLOG"
cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view 95 "*) printf 'CLOSED\n' ;;
    *" issue view "*)    printf 'OPEN\n' ;;
esac
exit 0
GHSTUB
chmod +x "$TMP/bin/gh"
scan_out="$(_gh_unlanded_scan 2>&1)"
if [[ "$scan_out" == *"resolved stale ask"* ]]; then
    ok "stale ask: resolution logged"
else
    bad "stale ask: resolution logged" "got: $scan_out"
fi
ask_status="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "${ASK_ID:-__none__}" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("status",""))' 2>/dev/null)"
is "stale ask: ask bead closed" closed "$ask_status"
if [ -e "$RUN/gh-closed/sp-scan5" ]; then
    ok "stale ask: gh-closed marker written for the bead"
else
    bad "stale ask: gh-closed marker written for the bead" "no marker at $RUN/gh-closed/sp-scan5"
fi

printf '\n16. _gh_unlanded_scan: a repo whose default branch is master behaves identically (law-the-base-branch-is-not-always-main):\n'
REPO2="$TMP/repo-master"
git init -q -b master "$REPO2"
git -C "$REPO2" commit -q --allow-empty -m "initial"
git -C "$REPO2" commit -q --allow-empty -m "spira: land sp-scan6"
printf 'master-fixture | %s\n' "$REPO2" >> "$SH/repo-map"
testdb_seed <<JSONL
{"id":"sp-scan6","title":"Scan master-branch landed bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:master-fixture"],"external_ref":"github:fixture/testrepo#96","updated_at":"2026-09-05T00:00:00Z"}
JSONL
printf 'RED 0000000000000000000000000000000000000000 %s conflicts-with-base\n' "$(date +%s)" \
    > "$RUN/landstate/sp-scan6"
: > "$GHLOG"
cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view "*) printf '{"state":"OPEN"}\n' ;;
esac
exit 0
GHSTUB
chmod +x "$TMP/bin/gh"
_gh_unlanded_scan >/dev/null 2>&1
if grep -q "issue close 96" "$GHLOG" 2>/dev/null; then
    ok "master-branch repo: landed bead closed out via the graph, not a hardcoded main"
else
    bad "master-branch repo: landed bead closed out via the graph, not a hardcoded main" \
        "issue 96 not closed — ghlog: $(cat "$GHLOG" 2>/dev/null)"
fi

printf '\n17. an open bead with an external_ref never produces an ask:\n'
testdb_seed <<JSONL
{"id":"sp-scan7","title":"Still-open bead","status":"open","issue_type":"task","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#97"}
JSONL
: > "$GHLOG"
MAIL_CALLS4="$TMP/mail.calls4"
: > "$MAIL_CALLS4"
cat > "$SH/mail.sh" <<MAILSTUB
#!/usr/bin/env bash
echo called >> $MAIL_CALLS4
exit 0
MAILSTUB
chmod +x "$SH/mail.sh"
_gh_unlanded_scan >/dev/null 2>&1
if grep -q "97" "$GHLOG" 2>/dev/null; then
    bad "open bead: no gh call for its issue" "GHLOG: $(cat "$GHLOG" 2>/dev/null)"
else
    ok "open bead: no gh call for its issue"
fi
if [ -s "$MAIL_CALLS4" ]; then
    bad "open bead: no ask sent" "mail.sh was called: $(cat "$MAIL_CALLS4")"
else
    ok "open bead: no ask sent"
fi

printf '\n18. gh_issue_ask_unlanded: answering the ask writes the durable marker (Defect 2 regression):\n'
# The tracking ask bead for sp-scan2 (github:fixture/testrepo#92) is OPEN, left behind
# by test 11's simulated mail reply. REGRESSION: without the fix, closing it — what the
# operator does to answer the mail — writes nothing, and the very next scan files an
# identical ask again (law-a-regression-test-must-be-seen-to-fail).
_ask2_subj="Close GitHub issue github:fixture/testrepo#92 for bead sp-scan2"
_ask2_id="$(bdjson list --status open --label needs-operator --limit 0 | python3 -c '
import sys, json
d = json.load(sys.stdin); rows = d if isinstance(d, list) else [d]
want = sys.argv[1]
for i in rows:
    if want in (i.get("title") or ""):
        print(i.get("id") or "")
        break' "$_ask2_subj" 2>/dev/null)"
if [ -z "$_ask2_id" ]; then
    bad "found the open tracking ask for sp-scan2" "none found — cannot run regression test"
else
    ok "found the open tracking ask for sp-scan2 ($_ask2_id)"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$_ask2_id" --reason-file - \
        <<< "answered: closed the github issue by hand" >/dev/null 2>&1
    : > "$GHLOG"
    cat > "$TMP/bin/gh" <<'GHSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GHLOG"
case " $* " in
    *" issue view "*) printf '{"state":"OPEN"}\n' ;;
esac
exit 0
GHSTUB
    chmod +x "$TMP/bin/gh"
    MAIL_CALLS5="$TMP/mail.calls5"
    : > "$MAIL_CALLS5"
    cat > "$SH/mail.sh" <<MAILSTUB
#!/usr/bin/env bash
echo called >> $MAIL_CALLS5
exit 0
MAILSTUB
    chmod +x "$SH/mail.sh"
    scan_out="$(_gh_unlanded_scan 2>&1)"
    if [ -e "$RUN/gh-closed/sp-scan2" ]; then
        ok "answering the ask wrote the gh-closed marker"
    else
        bad "answering the ask wrote the gh-closed marker" "no marker at $RUN/gh-closed/sp-scan2"
    fi
    if [ -s "$MAIL_CALLS5" ]; then
        bad "no new ask filed after the first was answered" "mail.sh was called: $(cat "$MAIL_CALLS5")"
    else
        ok "no new ask filed after the first was answered"
    fi
    want "log names the already-answered ask" "already answered" "$scan_out"
fi

tl_summary
