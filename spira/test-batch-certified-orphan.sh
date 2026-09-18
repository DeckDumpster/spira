#!/usr/bin/env bash
#
# test-batch-certified-orphan.sh — batch.sh logs and mails the operator when a CERTIFIED
#   landstate record has no corresponding branch ref.
#
# THE PROPERTY UNDER TEST (sp-e5ow0).  batch.sh's _certified_list iterates refs/heads/spira/*
# and checks the landstate.  A branch deleted while CERTIFIED simply falls out: no log line,
# no mail, no signal to the operator.  The fix adds _certified_orphans, which scans the
# landstate directory for CERTIFIED records and checks for the matching ref.  Missing → log
# line + operator mail.
#
# TWO CASES, TWO OUTCOMES:
#
#   sp-gone:  CERTIFIED landstate, no branch ref.  Batch must log a WARN and mail.
#
#   sp-good:  CERTIFIED landstate, branch exists.  Batch builds normally (positive control
#             that _certified_list still fires and the orphan scan does not block the batch).
#
# SEEN RED WITHOUT THE FIX.  Removing _certified_orphans from batch.sh causes the WARN and
# mail lines to be absent; the "want" assertions below then fail.
#
# defect: sp-e5ow0
# covers: spira/batch.sh spira/conf.sh spira/landing.sh
# scar: batch.sh iterated refs/heads/spira/* and silently dropped CERTIFIED landstate records whose ref was gone; no log line, no mail.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-certified-orphan
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchcertorphan || { echo "test-batch-certified-orphan: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
MAIL_LOG="$TMP/mail-log"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

# Write repo-map: queue mode.
cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# mail.sh stub — records calls for assertion.
cat > "$SH/mail.sh" <<MAIL
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"
: > "$MAIL_LOG"

# Forge stub — records pr-create calls and returns PR numbers.
FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

testdb_reset
testdb_seed <<JSONL
{"id":"sp-good","title":"certified branch with ref","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-17T00:00:00Z"}
JSONL

# ─── Fixture: sp-good — CERTIFIED landstate with an existing branch ───────────
git -C "$REPO" checkout -q -b spira/sp-good main
printf 'good\n' > "$REPO/sp-good.txt"
git -C "$REPO" add sp-good.txt && git -C "$REPO" commit -q -m "sp-good: work"
_good_tip="$(git -C "$REPO" rev-parse spira/sp-good)"
git -C "$REPO" checkout -q main
printf 'CERTIFIED %s %s\n' "$_good_tip" "$(date +%s)" > "$LANDSTATE/sp-good"

# ─── Fixture: sp-gone — CERTIFIED landstate but NO branch ref ─────────────────
# Plant only the landstate file; no refs/heads/spira/sp-gone exists.
printf 'CERTIFIED fakeshafakeshabrakeshabrakebrakefakeshabrakebra %s\n' "$(date +%s)" > "$LANDSTATE/sp-gone"

# ─── Fixture: sp-in-base — CERTIFIED orphan whose tip is already in origin/main ─
# Represents a member whose batch PR was merged before verdict.sh ran.
_base_tip="$(git -C "$REPO" rev-parse origin/main)"
printf 'CERTIFIED %s %s\n' "$_base_tip" "$(date +%s)" > "$LANDSTATE/sp-in-base"

echo "test-batch-certified-orphan.sh"

# =============================================================================
# POSITIVE CONTROL — sp-good is included in the batch (proves _certified_list
# still fires and the orphan scan does not suppress normal batching).
# =============================================================================
echo
echo "positive control — sp-good is included in a normal batch:"

out="$(batch "$REPONAME")"
rc=$?
is  "batch exits 0"                               0 "$rc"
want "batch opens a PR"                           "PR" "$out"
want "sp-good is a batch member"                  "sp-good" "$(cat "$QUEUEDIR/$REPONAME/open" 2>/dev/null)"

# =============================================================================
# ORPHAN DETECTION — sp-gone (CERTIFIED, no ref) produces a log line and a mail.
# =============================================================================
echo
echo "orphan detection — sp-gone (CERTIFIED, no ref) is logged and mailed:"

want "batch logs WARN for certified-orphan sp-gone" \
    "certified-orphan sp-gone" "$out"
want "mail.sh was called with the missing-branch subject" \
    "CERTIFIED branch" "$(cat "$MAIL_LOG" 2>/dev/null)"
nowant "sp-gone is not in the open batch"         "sp-gone" "$(cat "$QUEUEDIR/$REPONAME/open" 2>/dev/null)"

# =============================================================================
# LANDED VS LOST — orphan whose tip is in the base gets LANDED, not LOST.
#   POSITIVE CONTROL: sp-gone (fake tip, not in base) must be LOST; this proves
#   the ancestry check fires and would catch a code path that always writes LANDED.
# =============================================================================
echo
echo "LANDED vs LOST — orphan cleanup distinguishes tip-in-base from tip-gone:"

case "$(cat "$LANDSTATE/sp-in-base" 2>/dev/null)" in LANDED*)
    ok "orphan-in-base: sp-in-base marked LANDED" ;;
    *) bad "orphan-in-base: sp-in-base marked LANDED" \
           "got: $(cat "$LANDSTATE/sp-in-base" 2>/dev/null)" ;; esac

case "$(cat "$LANDSTATE/sp-gone" 2>/dev/null)" in LOST*)
    ok "orphan-not-in-base: sp-gone marked LOST" ;;
    *) bad "orphan-not-in-base: sp-gone marked LOST" \
           "got: $(cat "$LANDSTATE/sp-gone" 2>/dev/null)" ;; esac

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
