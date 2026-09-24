#!/usr/bin/env bash
#
# test-watchd-tail.sh — one reader per watcher, and the reader that lets go cleanly.
#
#   ./test-watchd-tail.sh
#
# WHAT IT HOLDS, AND THE OUTAGE IT COMES FROM. A watcher is a log plus ONE cursor. Two
# concurrent tails share that cursor, so each marks lines read on the other's behalf and both
# deliver every line — there is no coherent reading of two readers, which is why the
# cardinality is a lock and not advice.
#
# The advice is what was there, and it could not be followed. The session hook told a fresh
# context to "run ListAgents first, and attach only the streams not already listed there", but
# ListAgents enumerates agents and sessions and NO tool enumerates a session's own Monitors. So
# the check reported "none attached" on every context reset, and a session cleared several times
# accumulated seven readers: measured on 2026-09-08, twelve live processes on two logs, three of
# them orphaned `tail -F` dating back sixteen hours to sessions that had ended. Ryan's verdicts
# arrived in triplicate in the one place a duplicated line costs the most.
#
#   1. A SECOND TAIL REFUSES, and refuses fast enough that a Monitor ends rather than lingers.
#   2. THE EVENT IS DELIVERED ONCE. This is the acceptance criterion — the assertion is on the
#      count of deliveries of one line across BOTH readers, because a refusal that still let a
#      duplicate through would pass every other check here.
#   3. THE REFUSAL NAMES ITS OVERRIDE. A fence is a polite refusal, not a wall.
#   4. `--takeover` REALLY TRANSFERS IT. The incumbent's `tail` and `awk` outlive their wrapper
#      and hold the same lock through an inherited descriptor, so killing the wrapper alone is
#      a takeover that reports success and changes nothing. Asserted by the stream that follows.
#   5. THE LOCK IS RELEASED BY EXIT, never by a pid file. flock means a crash, a kill and a
#      session that simply went away all release it; a stale lock cannot exist.
#   6. NO ORPHANS. Killing a reader leaves no `tail -F` behind streaming into a closed pipe.
#
# It needs no database, no beads server and no systemd. It runs against a scratch SPIRA_RUN, so
# it cannot see, disturb or be confused by the operator's own watchers.
#
# defect: sp-monitor-dup
# tier: T2
# covers: spira/watchd.sh spira/hooks/session.sh UC-operator-channel-27
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
WATCHD="$HERE/watchd.sh"

has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }

TMP="$(mktemp -d)"; trap 'cleanup' EXIT
RUN="$TMP/run"; mkdir -p "$RUN/watchd"
LOG="$RUN/watchd/answers.log"
# The watcher under test is declared here, not borrowed from the shipped manifest: a row
# retired there left this suite tailing a name watchd no longer knows.
printf 'answers|log|%s\n' "$LOG" > "$TMP/watchers"
export SPIRA_WATCHERS="$TMP/watchers"

kids=""
cleanup() { for k in $kids; do kill -TERM "$k" 2>/dev/null; done; sleep 1; rm -rf "$TMP"; }
tail_bg() { SPIRA_RUN="$RUN" bash "$WATCHD" tail answers >"$1" 2>"$2" & kids="$kids $!"; echo $!; }
_tailers() { SPIRA_RUN="$RUN" bash "$WATCHD" tailers 2>/dev/null; }

# poll_for <timeout-s> <cmd...> -> 0 as soon as cmd succeeds, 1 on timeout. Every wait in this
# suite is for a condition, not a duration, so it returns as soon as the condition holds instead
# of paying a fixed sleep on every run, on every machine speed.
poll_for() {
    local timeout="$1"; shift
    local iters=$(( timeout * 5 )) i=0
    while [ "$i" -lt "$iters" ]; do
        "$@" && return 0
        sleep 0.2; i=$((i+1))
    done
    return 1
}
_tailer_shows() { case "$(_tailers)" in *"$1"*) return 0 ;; *) return 1 ;; esac; }
_count_at_least() {  # _count_at_least <needle> <want> <file...>
    local needle="$1" want="$2"; shift 2
    [ "$(cat "$@" 2>/dev/null | grep -c "$needle" || true)" -ge "$want" ]
}
_gone() { ! kill -0 "$1" 2>/dev/null; }
_no_readers() { [ "$(readers)" = 0 ]; }

# EVERY READER PROCESS UNDER THIS SCRATCH RUN, decided on argv[0] and then on the path in the
# arguments — never on a substring of the whole command line, which matches the shell that
# typed the pattern, this suite included (law-a-pattern-match-is-not-an-identity-check).
readers() {
    local d pid n=0
    for d in /proc/[0-9]*; do
        pid=${d#/proc/}; [ "$pid" = "$$" ] && continue
        [ -r "$d/cmdline" ] || continue
        local -a a=(); mapfile -d '' -t a < "$d/cmdline" 2>/dev/null || continue
        [ "${#a[@]}" -ge 2 ] || continue
        case "${a[0]##*/}" in tail|awk) ;; *) continue ;; esac
        printf '%s\n' "${a[@]}" | grep -qF "$RUN" && n=$((n+1))
    done
    echo "$n"
}

printf 'RYAN ANSWERED sp-aaa one\n' > "$LOG"

printf 'one reader per watcher\n'
p1="$(tail_bg "$TMP/a1.out" "$TMP/a1.err")"
poll_for 5 _tailer_shows "answers|$p1|"
has "the first reader holds the lock" "$(_tailers)" "answers|$p1|"

# THE REFUSAL IS TIMED, because a Monitor that hangs instead of exiting is the same duplicate
# by another name: it holds a session's task slot open forever waiting on a stream it will
# never get. `timeout 5` is the actual bound: rc=3 (refused) can only be seen here if the
# refusal returned before `timeout` fired rc=124, so no separate wall-clock assert is needed —
# a fixed "≤3s" threshold flaked under load without proving anything more than this does.
SPIRA_RUN="$RUN" timeout 5 bash "$WATCHD" tail answers >"$TMP/a2.out" 2>"$TMP/a2.err"; rc=$?
is  "a second tail refuses before the next event, not by hitting its timeout" "3" "$rc"
has "the refusal names the holder"           "$(cat "$TMP/a2.err")" "already being tailed by pid $p1"
has "and names its own override"             "$(cat "$TMP/a2.err")" "--takeover"

# THE ACCEPTANCE CRITERION. Emitted while both readers have been asked for, and counted across
# BOTH outputs: a refusal that still leaked a duplicate would satisfy every check above.
printf 'RYAN ANSWERED sp-ccc three\n' >> "$LOG"
poll_for 5 _count_at_least sp-ccc 1 "$TMP/a1.out" "$TMP/a2.out"
is  "the event is delivered exactly once"    "1" \
    "$(cat "$TMP/a1.out" "$TMP/a2.out" 2>/dev/null | grep -c sp-ccc)"

printf '\nthe override transfers the stream\n'
p2="$(SPIRA_RUN="$RUN" bash "$WATCHD" tail answers --takeover >"$TMP/a3.out" 2>"$TMP/a3.err" & kids="$kids $!"; echo $!)"
poll_for 8 _gone "$p1"
is  "the incumbent is gone"                  "no"  "$(kill -0 "$p1" 2>/dev/null && echo yes || echo no)"
# The takeover completes once p2 has the exclusive flock AND has written its own PID to the
# lock file, which can lag a loaded machine — poll rather than asserting at one point in time.
poll_for 8 _tailer_shows "answers|$p2|"
has "and the lock names the challenger"      "$(_tailers)" "answers|$p2|"
# THE REAL PROOF OF A TAKEOVER is the next event, not the pid in the lock file. The incumbent's
# `tail` and `awk` survive their wrapper and go on delivering; a takeover that killed only the
# wrapper would show a clean lock here and still hand this line to two readers.
printf 'RYAN ANSWERED sp-ddd four\n' >> "$LOG"
poll_for 5 _count_at_least sp-ddd 1 "$TMP/a1.out" "$TMP/a3.out"
is  "the next event goes only to the new reader" "1" \
    "$(cat "$TMP/a1.out" "$TMP/a3.out" 2>/dev/null | grep -c sp-ddd)"
# `grep -c` PRINTS THE COUNT AND THEN EXITS 1 ON ZERO, so a `|| echo 0` fallback appends a
# SECOND zero and the comparison fails on the one outcome it exists to confirm.
is  "and the old reader got none of it"          "0" \
    "$(grep -c sp-ddd "$TMP/a1.out" 2>/dev/null || true)"

printf '\nletting go\n'
kill -TERM "$p2" 2>/dev/null
poll_for 5 _no_readers
is  "no reader process is left behind"       "0"   "$(readers)"
is  "and the lock is free again"             ""    "$(_tailers)"
# A LOCK FILE LEFT ON DISK MUST NOT BLOCK ANYBODY. flock lives in the kernel, so the file
# outliving its holder is ordinary; a reader that read the pid inside it would refuse forever.
p4="$(tail_bg "$TMP/a4.out" "$TMP/a4.err")"
poll_for 5 _tailer_shows "answers|$p4|"
has "a later reader takes it cleanly"        "$(_tailers)" "answers|$p4|"
kill -TERM "$p4" 2>/dev/null
poll_for 3 _gone "$p4"

printf '\nthe flag is refused where it means nothing\n'
out="$(SPIRA_RUN="$RUN" bash "$WATCHD" peek answers --takeover 2>&1)"; rc=$?
is  "peek --takeover is refused"             "2"   "$rc"
has "and says why"                           "$out" "only applies to 'tail'"

tl_summary
