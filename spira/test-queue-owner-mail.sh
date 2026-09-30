#!/usr/bin/env bash
# test-queue-owner-mail.sh — every mutation the recorded owner makes to an open batch
# (eject, rebuild, force-push, merge) mails the concierge mailbox as a machine event
# (sp-91hb5): spira-mail-deliver.sh watches every registered mailbox and wakes its reader
# the instant new mail lands, so the Concierge learns of it within seconds rather than by
# polling. This suite checks the ejection case: an ordinary (non-refused) ejection by the
# batch's own owner must leave an UNREAD message in the concierge mailbox within 10s.
#
# SEEN RED on today's code: before sp-91hb5, queue.sh eject never wrote to any mailbox but
# operator's, so this suite fails here exactly as it would against the pre-fix tree — the
# assertion below is the one that was seen red before spira/lib.sh grew
# queue_notify_concierge and queue.sh eject started calling it.
#
# covers: queue/src/* spira/lib.sh mail/src/* spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

. "$HERE/testdb.sh"
testdb_require test-queue-owner-mail
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up owner-mail || { echo "test-queue-owner-mail: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null
cp -r "$HERE/mail" "$SH/mail"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift
case "\$cmd" in
    pr-close) printf '%s\tclose\n' "\${2:-}" >> "$FORGE_LOG" ;;
    *) : ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

MAIL="$RUN/mail"

queue() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${TESTDB_BD:-bd}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_MAIL="$MAIL" \
        SPIRA_HOME="$SH" command queue "$@"
}

concierge_unread() {
    SPIRA_HOME="$SH" SPIRA_MAIL="$MAIL" bash "$SH/mail" count concierge 2>/dev/null
}

echo "test-queue-owner-mail.sh"

testdb_seed <<'JSONL'
{"id":"sp-ownm1","title":"owner-mail-fixture","description":"fixture bead.","status":"closed","issue_type":"task","priority":2,"labels":[],"updated_at":"2026-09-24T00:00:00Z","closed_at":"2026-09-24T00:00:00Z"}
JSONL

base_sha="$(git -C "$REPO" rev-parse origin/main)"
bwt="$RUN/worktree/sp-ownm1"
git -C "$REPO" worktree add -q -b "spira/sp-ownm1" "$bwt" origin/main 2>/dev/null
printf 'sp-ownm1\n' > "$bwt/sp-ownm1.txt"
git -C "$bwt" add -A
git -C "$bwt" commit -q -m "sp-ownm1: work"
tip="$(git -C "$REPO" rev-parse "spira/sp-ownm1")"
stamp="20260927T000100Z"
batch_br="spira/queue/$stamp"
git -C "$REPO" branch -f "$batch_br" "$tip" >/dev/null
printf 'BATCHED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/sp-ownm1"

# No owner= line at all: the default, cooperative ownership every automatic path already
# shares (no concierge claim in effect) — the "owner" this suite's title refers to.
{
    printf 'pr=88\nhead=%s\nbase=%s\nmembers=sp-ownm1:%s\nopened=%s\nbranch=%s\n' \
        "$tip" "$base_sha" "$tip" "$(date +%s)" "$batch_br"
} > "$QUEUEDIR/$REPONAME/open"

echo
echo "an ordinary owner ejection produces an unread concierge mail within 10s:"

before="$(concierge_unread)"; before="${before:-0}"

queue eject sp-ownm1 "$REPONAME" >/dev/null 2>&1
rc=$?
wantrc "eject exits zero" 0 "$rc"

seen=0
deadline=$(( $(date +%s) + 10 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    after="$(concierge_unread)"; after="${after:-0}"
    if [ "$after" -gt "$before" ]; then seen=1; break; fi
    sleep 1
done

is "concierge mailbox has an unread message within 10s" "1" "$seen"

latest="$(ls -t "$MAIL/concierge/new" 2>/dev/null | head -1)"
body="$([ -n "$latest" ] && cat "$MAIL/concierge/new/$latest" || true)"
want "concierge mail names the ejected bead" "sp-ownm1" "$body"
want "concierge mail names the PR"           "88"       "$body"

echo
tl_summary
