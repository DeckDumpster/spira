#!/usr/bin/env bash
#
# test-sending-mode-filter.sh — sending.sh --skip-queue leaves a queue-mode (queue or
#   queue.local) repository's branches untouched across passes; --queue-only sweeps only
#   queue-mode repositories.
#
#   ./test-sending-mode-filter.sh
#
# WHY THIS EXISTS (sp-jci6o). A queue-mode repository's landed batch members are now reaped
# at landing itself (spira-lc close-on-land -> sending reap-landed-branch), so re-scanning
# every one of its branches in the per-pass sentinel Sending every two minutes rediscovers,
# by ancestry, what already left. --skip-queue removes that repository from the per-pass
# walk entirely; --queue-only is the daily straggler sweep that replaces it. This suite
# proves the two flags partition the REPOSITORY set, never a branch within one — a repo
# swept under either flag is judged exactly as sending.sh always judged it.
#
# tier: T1
# covers: sending/src/* spira/lib.sh
# hermetic-ok: a stub bd (always answers "no bead"), local git repos — no database, no
#   systemd, no real network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export SPIRA_CONF="$TMP/no-such-conf"

# THE STUB BD. Every branch built below is a zero-ahead ancestor of its base, so
# spira-lc content-landed approves the reap before send_disposition ever reads a bead — this suite
# needs no bead fixture at all, only a bd that answers quickly and does nothing destructive.
STUB_BD="$TMP/bd-stub"
cat > "$STUB_BD" <<'EOF'
#!/usr/bin/env bash
cmd=""; skip_next=0
for a in "$@"; do
    if [ "$skip_next" = 1 ]; then skip_next=0; continue; fi
    case "$a" in
        -C)   skip_next=1 ;;
        show) cmd=show ;;
        list) cmd=list ;;
    esac
done
[ "$cmd" = show ] && printf '[]\n'
# The positive control (spira_db_reachable): the store lists at least one bead.
[ "$cmd" = list ] && printf '[{"id":"sp-any"}]\n'
exit 0
EOF
chmod +x "$STUB_BD"
export SPIRA_BD="$STUB_BD" SPIRA_DB="$TMP/no-such-db"

# --------------------------------------------------------------------------------------
# THREE REPOSITORIES: queue, queue.local and push. Each gets a bare remote so the
# remote-branch-delete step has something to reach; a zero-ahead branch off main in each,
# built fresh before every invocation so a repo's branch surviving one pass and not the
# next is proof the filter — not luck — decided it.
# --------------------------------------------------------------------------------------
mk_repo() {   # mk_repo <dir>
    local d="$1"
    git init -q -b main "$d"
    git -C "$d" commit -q --allow-empty -m base
    git -C "$d" remote add origin "${d}.git"
    git init -q --bare -b main "${d}.git"
    git -C "$d" push -q origin main
    git -C "$d" remote set-head origin main
}
QREPO="$TMP/qrepo"; PREPO="$TMP/prepo"; QLREPO="$TMP/qlrepo"
mk_repo "$QREPO"
mk_repo "$PREPO"
mk_repo "$QLREPO"
QNAME="$(basename "$QREPO")"; PNAME="$(basename "$PREPO")"; QLNAME="$(basename "$QLREPO")"

RUN="$TMP/run"; mkdir -p "$RUN/worktree" "$RUN/landstate"
export SPIRA_RUN="$RUN" SPIRA_REAPLOG="$RUN/reap.log"
printf '%s | %s | queue | | |\n%s | %s | push | | |\n%s | %s | queue.local | | |\n' \
    "$QNAME" "$QREPO" "$PNAME" "$PREPO" "$QLNAME" "$QLREPO" > "$TMP/repo-map"
export SPIRA_REPO_MAP="$TMP/repo-map" SPIRA_HOME_REPO="$PNAME" SPIRA_REPO="$PREPO"

sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$STUB_BD" \
    SPIRA_REPO="$PREPO" SPIRA_HOME_REPO="$PNAME" SPIRA_REPO_MAP="$TMP/repo-map" \
        command sending --no-fetch "$@" 2>&1
}
branch_exists() {   # branch_exists <repo> <branch>
    git -C "$1" show-ref --verify -q "refs/heads/$2" 2>/dev/null
}

echo "test-sending-mode-filter.sh"

# ======================================================================================
echo
echo "--skip-queue: the queue-mode repo's branch survives one pass, the push-mode repo's does not:"
# ======================================================================================
git -C "$QREPO" branch spira/sp-skip-q main
git -C "$PREPO" branch spira/sp-skip-p main
git -C "$QLREPO" branch spira/sp-skip-ql main

out1="$(sending --skip-queue)"

want "push-mode repo's branch is SENT under --skip-queue" "SENT sp-skip-p" "$out1"
is   "push-mode branch is gone" 1 "$(branch_exists "$PREPO" spira/sp-skip-p; echo $?)"

nowant "queue-mode repo's branch is not mentioned under --skip-queue" "sp-skip-q" "$out1"
is   "queue-mode branch still exists after --skip-queue" 0 \
    "$(branch_exists "$QREPO" spira/sp-skip-q; echo $?)"

# sp-ksmdb: queue.local reaps via spira-lc close-on-land exactly like queue mode, so the
# per-pass sentinel sweep (--skip-queue) must leave its branches alone too — landed-but-
# unpublished work there is still needed by publish-red attribution.
nowant "queue.local repo's branch is not mentioned under --skip-queue" "sp-skip-ql" "$out1"
is   "queue.local branch still exists after --skip-queue" 0 \
    "$(branch_exists "$QLREPO" spira/sp-skip-ql; echo $?)"

# A second --skip-queue pass changes nothing further for the queue-mode repo: this is not
# a one-time exemption but a standing partition of the repository set.
out1b="$(sending --skip-queue)"
nowant "a second --skip-queue pass still never mentions the queue-mode repo's branch" \
    "sp-skip-q" "$out1b"
is   "queue-mode branch still exists after a second --skip-queue pass" 0 \
    "$(branch_exists "$QREPO" spira/sp-skip-q; echo $?)"
nowant "a second --skip-queue pass still never mentions the queue.local repo's branch" \
    "sp-skip-ql" "$out1b"
is   "queue.local branch still exists after a second --skip-queue pass" 0 \
    "$(branch_exists "$QLREPO" spira/sp-skip-ql; echo $?)"

# ======================================================================================
echo
echo "--queue-only: the queue-mode repo (including the branch --skip-queue left standing) is swept, the push-mode repo is not:"
# ======================================================================================
git -C "$QREPO" branch spira/sp-only-q main
git -C "$PREPO" branch spira/sp-only-p main
git -C "$QLREPO" branch spira/sp-only-ql main

out2="$(sending --queue-only)"

want "sp-skip-q (left standing by --skip-queue) is SENT under --queue-only" \
    "SENT sp-skip-q" "$out2"
want "sp-only-q is SENT under --queue-only" "SENT sp-only-q" "$out2"
is   "sp-skip-q is now gone"  1 "$(branch_exists "$QREPO" spira/sp-skip-q; echo $?)"
is   "sp-only-q is now gone"  1 "$(branch_exists "$QREPO" spira/sp-only-q; echo $?)"

want "sp-skip-ql (left standing by --skip-queue) is SENT under --queue-only" \
    "SENT sp-skip-ql" "$out2"
want "sp-only-ql is SENT under --queue-only" "SENT sp-only-ql" "$out2"
is   "sp-skip-ql is now gone" 1 "$(branch_exists "$QLREPO" spira/sp-skip-ql; echo $?)"
is   "sp-only-ql is now gone" 1 "$(branch_exists "$QLREPO" spira/sp-only-ql; echo $?)"

nowant "push-mode repo's branch is not mentioned under --queue-only" "sp-only-p" "$out2"
is   "push-mode branch still exists after --queue-only" 0 \
    "$(branch_exists "$PREPO" spira/sp-only-p; echo $?)"

# ======================================================================================
echo
echo "--skip-queue and --queue-only together refuse rather than silently picking one:"
# ======================================================================================
out3="$(sending --skip-queue --queue-only)"; rc3=$?
is   "both flags together exits non-zero" 1 "$([ "$rc3" -ne 0 ] && echo 1 || echo 0)"
want "both flags together names the conflict" "mutually exclusive" "$out3"

tl_summary
