#!/usr/bin/env bash
#
# test-gh-issue-closeout.sh — a thin real-sender smoke test of `gh-intake closeout`,
# `gh-intake unlanded-scan` and `gh-intake backfill` (sp-j3fim, "wave 4.31": the
# GitHub-closeout family (AB) ported natively from spira/lib.sh into gh-intake/src/
# closeout.rs). The exhaustive logic — every parsing edge case, both directions of the
# operator-ask dedupe, the mail-body text, the ancestry fallbacks — is covered by
# `cargo test -p gh-intake` against fakes; this suite exists only to prove the compiled
# binary's own wiring: argv, env resolution, and the real `ghq`/`mail`/`bd`/git subprocess
# boundaries, the same role test-gh-intake.sh plays for the ingest half.
#
# NEVER TOUCHES REAL GITHUB: `ghq` (bead::bdq's wrapper) is pointed at a stub script via
# SPIRA_GH, never at the real `gh` binary.
#
# tier: T1
# covers: gh-intake/src/* spira/lib.sh
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

# --- gh stub: ghq (bead::bdq's wrapper around \$SPIRA_GH) never reaches a real repo ---
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

# --- mail stub: records a call and, with --bead, wires an ask the way the real `mail`
# binary does (dep relate), so the dedupe tests below see the same tracking bead a real
# answered ask would leave behind. $MAILLOG/$SPIRA_DB/$SPIRA_BD/$SPIRA_ASK_LABEL are read
# from this stub's own (inherited) environment at run time — the heredoc below is quoted
# so none of them expand at generation time. ---
MAILLOG="$TMP/mail.log"
export MAILLOG
cat > "$TMP/bin/mail" <<'MAILSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$MAILLOG"
_bead=""; _subj=""
while [ $# -gt 0 ]; do
    case "$1" in
        --bead)    _bead="$2"; shift 2 ;;
        --subject) _subj="$2"; shift 2 ;;
        *) shift ;;
    esac
done
body="$(cat)"
if [ -n "$_bead" ] && [ -n "${SPIRA_DB:-}" ] && [ -n "$_subj" ]; then
    _lab="${SPIRA_ASK_LABEL:-needs-operator}"
    _ask="$(printf '%s\n' "$body" | "${SPIRA_BD:-bd}" -C "$SPIRA_DB" create "$_subj" \
        -l "$_lab,overseer" --type decision --body-file - --silent 2>/dev/null)" || _ask=""
    [ -n "$_ask" ] && "${SPIRA_BD:-bd}" -C "$SPIRA_DB" dep relate "$_ask" "$_bead" >/dev/null 2>&1
fi
exit 0
MAILSTUB
chmod +x "$TMP/bin/mail"
export PATH="$TMP/bin:$PATH"

# The lifecycle machine mirrors the throwaway store (sp-mve9i: which GitHub beads the
# builder handed on is the lifecycle state, not bd status): a bead bd shows closed is one its
# builder submitted — SUBMITTED, past the builder, with no delivery record — and an open one
# is READY. Whether it landed is the git evidence each case plants, as before.
lc_mirror_bd "$TMP/lcm"; unset SPIRA_LC_BIN   # the PATH stub below is the one gh-intake runs
cat > "$TMP/bin/spira-lc" <<LCSTUB
#!/usr/bin/env bash
LC_MIRROR_CLOSED=SUBMITTED exec "$TMP/lcm/spira-lc" "\$@"
LCSTUB
chmod +x "$TMP/bin/spira-lc"

# --- spira environment: every var gh-intake resolves config from, set explicitly so no
# in-process resolve ever needs a real conf.d (none exists in this fixture). ---
RUN="$TMP/run"
mkdir -p "$RUN/gh-closed" "$RUN/landstate"
export SPIRA_RUN="$RUN"
export SPIRA_HOME="$TMP"
export SPIRA_HOME_REPO=fixture
export SPIRA_REPO="$REPO"
export SPIRA_REPO_DERIVED="$REPO"
export SPIRA_ASK_LABEL=needs-operator

# The repo→path map, deliberately not named like the production config file (this is a
# fixture gh-intake resolves in-process through spira-config; no fence applies).
MAPFILE="$TMP/reposmap"
printf 'fixture | %s\n' "$REPO" > "$MAPFILE"
export SPIRA_REPO_MAP="$MAPFILE"

# --- bd fixture ---
testdb_seed <<JSONL
{"id":"sp-tgh1","title":"Fix issue 1","status":"open","issue_type":"bug","labels":["spira","plan"],"external_ref":"github:fixture/testrepo#1","updated_at":"2026-09-05T00:00:00Z"}
{"id":"sp-tgh3","title":"No external ref","status":"open","issue_type":"bug","labels":["spira","plan"],"updated_at":"2026-09-05T00:00:00Z"}
JSONL
BEAD1=sp-tgh1
BEAD3=sp-tgh3
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD1" --reason-file - <<< "done" 2>/dev/null || true
"${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$BEAD3" --reason-file - <<< "done" 2>/dev/null || true

printf '1. gh-intake closeout comments and closes a landed issue:\n'
: > "$GHLOG"
out="$(gh-intake closeout "$BEAD1" "$LANDED_SHA" "$REPO" 2>&1)"
if grep -q "issue comment" "$GHLOG" 2>/dev/null; then
    ok "gh comment was posted"
else
    bad "gh comment was posted" "no 'issue comment' call — GHLOG: $(cat "$GHLOG" 2>/dev/null), out: $out"
fi
if grep -q "issue close" "$GHLOG" 2>/dev/null; then
    ok "gh issue was closed"
else
    bad "gh issue was closed" "no 'issue close' call"
fi
SHORT_SHA="$(git -C "$REPO" rev-parse --short "$LANDED_SHA")"
# The comment body carries its own blank line before the link, so the gh stub's one
# `printf '%s\n' "$*"` call spans several lines in GHLOG — search the whole file, not
# just the "issue comment" line.
if grep -q "issue comment" "$GHLOG" 2>/dev/null && grep -q "https://github.com/fixture/testrepo/commit/" "$GHLOG" 2>/dev/null; then
    ok "comment contains a commit link"
else
    bad "comment contains a commit link" "GHLOG: $(cat "$GHLOG" 2>/dev/null)"
fi
if [ -e "$RUN/gh-closed/$BEAD1" ]; then
    ok "close marker written"
else
    bad "close marker written" "no marker at $RUN/gh-closed/$BEAD1"
fi

printf '\n2. a second call is idempotent (no gh call):\n'
: > "$GHLOG"
gh-intake closeout "$BEAD1" "$LANDED_SHA" "$REPO" >/dev/null 2>&1
if [ -s "$GHLOG" ]; then
    bad "no gh call on second invocation" "got: $(cat "$GHLOG")"
else
    ok "no gh call on second invocation"
fi

printf '\n3. a bead with no github ref is a no-op:\n'
: > "$GHLOG"
gh-intake closeout "$BEAD3" "$LANDED_SHA" "$REPO" >/dev/null 2>&1
if [ -s "$GHLOG" ]; then
    bad "no gh call for non-github bead" "got: $(cat "$GHLOG")"
else
    ok "no gh call for non-github bead"
fi

printf '\n4. gh-intake backfill --dry-run finds the landed bead by ancestry and makes no gh calls:\n'
testdb_seed <<JSONL
{"id":"sp-bf1","title":"Backfill candidate","status":"closed","issue_type":"bug","labels":["spira","plan","repo:fixture"],"external_ref":"github:fixture/testrepo#2","updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z"}
JSONL
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-bf1"
: > "$GHLOG"
out="$(gh-intake backfill --dry-run 2>&1)"
if grep -q "issue" "$GHLOG" 2>/dev/null; then
    bad "dry-run makes no gh calls" "got: $(cat "$GHLOG")"
else
    ok "dry-run makes no gh calls"
fi
if [[ "$out" == *"would close github:fixture/testrepo#2 (sp-bf1)"* ]]; then
    ok "dry-run reports what would be closed"
else
    bad "dry-run reports what would be closed" "got: $out"
fi

printf '\n5. gh-intake backfill (real pass) closes the issue:\n'
: > "$GHLOG"
out="$(gh-intake backfill 2>&1)"
if grep -q "issue close 2" "$GHLOG" 2>/dev/null; then
    ok "backfill closed the bead's issue"
else
    bad "backfill closed the bead's issue" "GHLOG: $(cat "$GHLOG" 2>/dev/null), out: $out"
fi
if [ -e "$RUN/gh-closed/sp-bf1" ]; then
    ok "backfill wrote the close marker"
else
    bad "backfill wrote the close marker" "no marker at $RUN/gh-closed/sp-bf1"
fi

printf '\n6. gh-intake unlanded-scan asks about a truly unlanded bead, and the duplicate is deduped:\n'
testdb_seed <<JSONL
{"id":"sp-scan1","title":"Truly unlanded","status":"closed","issue_type":"task","labels":["spira","plan","repo:fixture"],"external_ref":"github:fixture/testrepo#3","updated_at":"2026-09-05T00:00:00Z","closed_at":"2026-09-05T00:00:00Z"}
JSONL
# Its builder finished it and the machine has it over without a delivery (DONE): neither
# landed nor in flight, which is what the scan asks about.
echo "sp-scan1 DONE" >> "$TMP/lcm/states"
: > "$GHLOG"; : > "$MAILLOG"
scan_out="$(gh-intake unlanded-scan 2>&1)"
want "direction 1 — a missed ask is actually sent" "asked operator about github:fixture/testrepo#3" "$scan_out"
if [ -s "$MAILLOG" ]; then
    ok "mail was called once for the unlanded bead"
else
    bad "mail was called once for the unlanded bead" "mail.log empty"
fi

: > "$MAILLOG"
scan_out2="$(gh-intake unlanded-scan 2>&1)"
if [ -z "$scan_out2" ] && [ ! -s "$MAILLOG" ]; then
    ok "direction 2 — the duplicate ask is deduped, no second mail"
else
    bad "direction 2 — the duplicate ask is deduped, no second mail" "scan_out2: $scan_out2, mail.log: $(cat "$MAILLOG" 2>/dev/null)"
fi

printf '\n7. answering the tracking ask lets the scan write the durable marker instead of re-asking:\n'
ASK_SUBJ="Close GitHub issue github:fixture/testrepo#3 for bead sp-scan1"
ASK_ID="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --status open --label needs-operator --limit 0 --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin); rows = d if isinstance(d, list) else [d]
want = sys.argv[1]
for i in rows:
    if want == (i.get("title") or ""):
        print(i.get("id") or "")
        break' "$ASK_SUBJ" 2>/dev/null)"
if [ -z "$ASK_ID" ]; then
    bad "found the open tracking ask" "none found — cannot run the regression check"
else
    ok "found the open tracking ask ($ASK_ID)"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$ASK_ID" --reason-file - <<< "answered: closed by hand" >/dev/null 2>&1
    : > "$MAILLOG"
    scan_out3="$(gh-intake unlanded-scan 2>&1)"
    if [ -e "$RUN/gh-closed/sp-scan1" ]; then
        ok "answering the ask wrote the durable marker"
    else
        bad "answering the ask wrote the durable marker" "no marker at $RUN/gh-closed/sp-scan1"
    fi
    if [ -s "$MAILLOG" ]; then
        bad "no new ask filed after the first was answered" "mail.log: $(cat "$MAILLOG")"
    else
        ok "no new ask filed after the first was answered"
    fi
    want "log names the already-answered ask" "already answered" "$scan_out3"
fi

tl_summary
