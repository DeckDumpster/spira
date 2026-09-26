#!/usr/bin/env bash
# test-queue-eject-mail.sh — ejection mail carries the ejected bead's rendered details.
#
# verdict.sh passes --bead to the REAL mail.sh (not a stub) and lets it render the
# block (law-a-bead-reference-carries-its-details); this suite no longer hand-checks
# a title/description verdict.sh used to build itself.
#
# Two cases:
#   1. bd show succeeds: rendered block carries title, status, priority.
#      POSITIVE CONTROL: bead sp-ejm01 is seeded with known values; only they can
#      cause the assertions to pass.
#   2. bd show fails (bead not in db): rendered block is "unresolved: sp-ejm02";
#      mail is still sent.
#
# covers: spira/verdict.sh spira/mail.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

. "$HERE/testdb.sh"
testdb_require test-queue-eject-mail
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up eject-mail || { echo "test-queue-eject-mail: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
FORGE_STATUS_FILE="$TMP/forge-status"
FORGE_RUN_METADATA_FILE="$TMP/forge-run-metadata"
FORGE_LOG="$TMP/forge-log"
export FORGE_STATUS_FILE FORGE_RUN_METADATA_FILE FORGE_LOG

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift
case "$cmd" in
    check-status)    cat "${FORGE_STATUS_FILE}" 2>/dev/null || printf 'pending\n' ;;
    run-id)          printf 'run-99\n' ;;
    run-metadata)    cat "${FORGE_RUN_METADATA_FILE}" 2>/dev/null || true ;;
    run-cancel)      printf '%s\tcancel\n' "${1:-}" >> "$FORGE_LOG" ;;
    workflow-rerun)  printf '%s\trerun\n'  "${1:-}" >> "$FORGE_LOG" ;;
    pr-close)        printf '%s\tclose\n'  "${1:-}" >> "$FORGE_LOG" ;;
    *) printf 'forge: unknown command: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

cp -r "$HERE/mail" "$SH/mail"

cat > "$SH/suites.sh" <<'SUITES'
#!/usr/bin/env bash
true
SUITES
chmod +x "$SH/suites.sh"

cat > "$SH/repro-always-red.sh" <<'REPRO'
#!/usr/bin/env bash
exit 1
REPRO
chmod +x "$SH/repro-always-red.sh"

batch_file() { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }

MAIL="$RUN/mail"

verdict() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${TESTDB_BD:-bd}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_CI_MAXSEC=3600 \
    SPIRA_QUEUE_CI_IDLE_SEC=600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro-always-red.sh" \
    SPIRA_MAIL="$MAIL" \
    SPIRA_MAIL_REPEAT_CONSIDERED="test-queue-eject-mail" \
        bash "$SH/verdict.sh" "$@" 2>&1
}

latest_operator_mail() {
    local f
    f="$(ls -t "$MAIL/operator/new" 2>/dev/null | head -1)"
    [ -n "$f" ] && cat "$MAIL/operator/new/$f"
}

echo "test-queue-eject-mail.sh"

# Seed bead with known title, status and priority.
testdb_seed <<'JSONL'
{"id":"sp-ejm01","title":"eject-mail-test-title","description":"First paragraph here.\n\nSecond paragraph.","status":"closed","issue_type":"task","priority":2,"labels":[],"updated_at":"2026-09-24T00:00:00Z","closed_at":"2026-09-24T00:00:00Z"}
JSONL

# ==========================================================================
# 1. bd show succeeds: rendered block carries title, status, priority.
#    POSITIVE CONTROL: only sp-ejm01's seeded values match the assertions.
# ==========================================================================
echo
echo "ejection mail's rendered block carries bead title, status, priority:"

base_sha="$(git -C "$REPO" rev-parse origin/main)"
bwt="$RUN/worktree/sp-ejm01"
git -C "$REPO" worktree add -q -b "spira/sp-ejm01" "$bwt" origin/main 2>/dev/null || true
printf 'sp-ejm01\n' > "$bwt/sp-ejm01.txt"
git -C "$bwt" add -A
git -C "$bwt" commit -q -m "sp-ejm01: work"
tip="$(git -C "$REPO" rev-parse "spira/sp-ejm01")"
printf 'BATCHED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/sp-ejm01"
{ printf 'pr=42\nhead=%s\nbase=%s\nmembers=sp-ejm01:%s\nopened=%s\n' \
    "$tip" "$base_sha" "$tip" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-ejm-suite.sh\n' > "$FORGE_STATUS_FILE"
rm -rf "$MAIL"

verdict "$REPONAME" >/dev/null

mail="$(latest_operator_mail)"
want "rendered block leads with the bead id and title" "sp-ejm01: eject-mail-test-title" "$mail"
# verdict.sh reopens the bead (queue-eject) before mailing, so the render sees the
# post-reopen status — open, not the closed state it was seeded with.
want "rendered block carries status"                    "Status: open"                   "$mail"
want "rendered block carries priority"                   "Priority: P2"                   "$mail"

rm -f "$(batch_file)" "$LANDSTATE/sp-ejm01"
git -C "$REPO" worktree remove -f "$bwt" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-ejm01" 2>/dev/null || true
git -C "$REPO" worktree prune 2>/dev/null || true

# ==========================================================================
# 2. bd show fails (bead not in db): rendered block is "unresolved: <id>".
#    POSITIVE CONTROL: sp-ejm01 (case 1) proves the bd-show path renders real
#    values; sp-ejm02 is not seeded, so bd show fails, triggering "unresolved".
# ==========================================================================
echo
echo "ejection mail renders 'unresolved: <id>' when bd show fails, and still sends:"

base_sha="$(git -C "$REPO" rev-parse origin/main)"
bwt2="$RUN/worktree/sp-ejm02"
git -C "$REPO" worktree add -q -b "spira/sp-ejm02" "$bwt2" origin/main 2>/dev/null || true
printf 'sp-ejm02\n' > "$bwt2/sp-ejm02.txt"
git -C "$bwt2" add -A
git -C "$bwt2" commit -q -m "sp-ejm02: work"
tip2="$(git -C "$REPO" rev-parse "spira/sp-ejm02")"
printf 'BATCHED %s %s\n' "$tip2" "$(date +%s)" > "$LANDSTATE/sp-ejm02"
{ printf 'pr=43\nhead=%s\nbase=%s\nmembers=sp-ejm02:%s\nopened=%s\n' \
    "$tip2" "$base_sha" "$tip2" "$(date +%s)"; } > "$(batch_file)"
printf 'red\nred-suite: test-ejm-suite.sh\n' > "$FORGE_STATUS_FILE"
rm -rf "$MAIL"

verdict "$REPONAME" >/dev/null

mail2="$(latest_operator_mail)"
want "fallback: mail sent"                    "ejected from the merge queue" "$mail2"
want "fallback: body renders unresolved bead" "unresolved: sp-ejm02"         "$mail2"

rm -f "$(batch_file)" "$LANDSTATE/sp-ejm02"
git -C "$REPO" worktree remove -f "$bwt2" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-ejm02" 2>/dev/null || true

echo
tl_summary
