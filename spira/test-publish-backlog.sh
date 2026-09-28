#!/usr/bin/env bash
#
# test-publish-backlog.sh — sp-1mkcs: publish-backlog.sh, the unpublished-backlog alarm for
# queue.local repos (row 5 of the local/main design). Below both SPIRA_LOCAL_BACKLOG_COUNT
# and SPIRA_LOCAL_BACKLOG_AGE a tick is silent; past either, it mails the concierge exactly
# once (the crossing, not the steady state) and recovers the same way when the publish queue
# catches back up. A queue.forge repo is never touched — the alarm is queue.local-only.
#
# THE POSITIVE CONTROL FIRST (test 1): a repo already under threshold proves silence is the
# fixture working, not the check being unable to fire at all
# (law-absence-needs-a-positive-control).
#
# REAL MAIL, NOT A STUB: mail.sh runs for real so a test can assert the actual inbox line
# landed in $RUN/mail/concierge/new/*, the same seam test-pr-notify.sh uses for its own
# "every transition is mailed" assertions.
#
# tier: T1
# covers: spira/publish-backlog.sh spira/watchers spira/conf.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-publish-backlog.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/mail" "$SH/mail" 2>/dev/null || true
chmod +x "$SH"/*.sh 2>/dev/null || true

# --- fixture repo 1: queue.local, the alarm's own target -------------------------------
REMOTE="$TMP/remote.git"; REPO="$TMP/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" branch local/main main

# --- fixture repo 2: queue.forge, the negative control (never scanned) -----------------
FREPO="$TMP/frepo"
git init -q -b main "$FREPO"
git -C "$FREPO" commit -q --allow-empty -m base

RUN="$TMP/run"; mkdir -p "$RUN/worktree"
RMAP="$TMP/repo-map"
{
    printf 'fixlocal | %s | queue.local | local/main | | |\n' "$REPO"
    printf 'fixforge | %s | queue | main | | |\n'              "$FREPO"
} > "$RMAP"

pb() {
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_HOME_REPO=fixlocal \
    SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN" \
    SPIRA_REPO_MAP="$RMAP" \
    SPIRA_LOCAL_BACKLOG_COUNT="${BKCOUNT:-50}" \
    SPIRA_LOCAL_BACKLOG_AGE="${BKAGE:-10800}" \
        bash "$SH/publish-backlog.sh" "$@" 2>&1
}
mail_count() { ls "$RUN/mail/concierge/new" 2>/dev/null | wc -l | tr -d ' '; }
mail_grep()  { grep -l "$1" "$RUN"/mail/concierge/new/* 2>/dev/null | wc -l | tr -d ' '; }
clear_mail() { rm -f "$RUN"/mail/concierge/new/* 2>/dev/null; }
publish_commit() {   # publish_commit <msg> [iso-date] -> a commit on local/main, now unless dated
    git -C "$REPO" checkout -q local/main
    printf '%s\n' "$1" >> "$REPO/f.txt"
    git -C "$REPO" add f.txt
    if [ -n "${2:-}" ]; then
        GIT_AUTHOR_DATE="$2" GIT_COMMITTER_DATE="$2" git -C "$REPO" commit -q -m "$1"
    else
        git -C "$REPO" commit -q -m "$1"
    fi
}

# ============================================================================
echo
echo "1 — below both thresholds: silent (positive control: the fixture CAN mail — see test 2)"
# ============================================================================
BKCOUNT=50 BKAGE=10800
publish_commit "c1"
out="$(pb --show)"; rc=$?
is  "1: exit 0"                     "0" "$rc"
nowant "1: nothing over threshold reported" "OVER" "$out"
is  "1: no mail sent"               "0" "$(mail_count)"

# ============================================================================
echo
echo "2 — count threshold crossed: OVER is reported and mailed, exactly once"
# ============================================================================
clear_mail
BKCOUNT=0 BKAGE=10800
out="$(pb --show)"; rc=$?
is   "2: exit 0"                              "0" "$rc"
want "2: OVER is reported for fixlocal"       "OVER fixlocal" "$out"
want "2: names the unpublished count"         "1 unpublished commit" "$out"
is   "2: exactly one mail sent"               "1" "$(mail_count)"
is   "2: the mail names the crossing"         "1" "$(mail_grep "OVER fixlocal")"

clear_mail
out2="$(pb --show)"
nowant "2b: a second tick, still over, reports nothing new" "OVER" "$out2"
is     "2b: and sends no further mail (transitions, not state)" "0" "$(mail_count)"

# ============================================================================
echo
echo "3 — recovery: publishing catches the forge up, and CLEAR fires once"
# ============================================================================
clear_mail
git -C "$REPO" push -q origin local/main:main
out="$(pb --show)"; rc=$?
is   "3: exit 0"                          "0" "$rc"
want "3: CLEAR is reported for fixlocal"  "CLEAR fixlocal" "$out"
is   "3: exactly one mail sent"           "1" "$(mail_count)"

clear_mail
out2="$(pb --show)"
is "3b: below threshold and already clear — silent again" "" "$out2"
is "3b: no mail on the steady clear state"                 "0" "$(mail_count)"

# ============================================================================
echo
echo "4 — age threshold alone (count never trips) crosses it too"
# ============================================================================
clear_mail
publish_commit "old" "2000-01-01T00:00:00Z"
BKCOUNT=1000 BKAGE=60
out="$(pb --show)"; rc=$?
want "4: OVER fires from age alone"          "OVER fixlocal" "$out"
want "4: only 1 unpublished commit (not count)" "1 unpublished commit" "$out"
is   "4: mailed"                             "1" "$(mail_count)"

# ============================================================================
echo
echo "5 — a queue.forge repo is never scanned: no crash, no mail naming it, whatever its state"
# ============================================================================
clear_mail
rm -rf "$RUN/watchd"   # reset transition state so fixlocal alarms fresh THIS tick — proof mail is flowing
BKCOUNT=0 BKAGE=0      # would alarm on anything, if fixforge were scanned
out="$(pb --show)"; rc=$?
is     "5: exit 0"                                              "0" "$rc"
want   "5: fixlocal still alarms — mail is flowing this tick"   "OVER fixlocal" "$out"
nowant "5: fixforge never named"                                "fixforge" "$out"
is     "5: no mail mentions fixforge"                           "0" "$(mail_grep fixforge)"

# ============================================================================
echo
echo "6 — watch --ticks and health"
# ============================================================================
rm -rf "$RUN/watchd"
SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixlocal SPIRA_REPO="$REPO" \
SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$RMAP" SPIRA_LOCAL_BACKLOG_COUNT=1000 SPIRA_LOCAL_BACKLOG_AGE=1000000 \
    bash "$SH/publish-backlog.sh" watch --interval 5 --ticks 1 >/dev/null 2>&1
[ -f "$RUN/watchd/publish-backlog.health" ] && ok "6: watch writes a health file" \
    || bad "6: watch writes a health file" "missing"
SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixlocal SPIRA_REPO="$REPO" \
SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$RMAP" \
    bash "$SH/publish-backlog.sh" health; rc=$?
is "6: health is ok right after a tick" "0" "$rc"

rm -rf "$RUN/watchd"
SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_HOME_REPO=fixlocal SPIRA_REPO="$REPO" \
SPIRA_RUN="$RUN" SPIRA_REPO_MAP="$RMAP" \
    bash "$SH/publish-backlog.sh" health >/dev/null 2>&1; rc=$?
is "6b: health fails when never polled" "1" "$rc"

tl_summary
