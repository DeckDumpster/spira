#!/usr/bin/env bash
#
# gate-run.sh — run the landing gate so that no single foreground call can outlive the
# ceiling an agent's tool puts on one command.
#
#   gate-run.sh <branch> [repo-name]      start it if nothing is running, then wait up to
#                                         SPIRA_GATE_POLL seconds and report
#   gate-run.sh --status <branch> [repo]  answer now, waiting for nothing
#   gate-run.sh --exec   <branch> [repo]  the detached run itself; not for hand use
#
# Exit codes, and they are the interface:
#
#   0  the gate passed              1  the gate failed, or could not be run
#   2  still deciding — call the same command again
#   3  (--status only) no gate is running for that branch and none has finished
#
# WHY THIS EXISTS. An agent's Bash tool moves a foreground command to the background at a
# fixed ceiling and hands the session back a task id. The gate outgrew that ceiling, so the
# aeon that ran it never saw a verdict: it ended its turn to wait, ending its turn ended the
# session, and the bead was released in_progress WITH AN ATTEMPT CHARGED for a race it did
# not lose. A fresh aeon was then summoned onto the same bead to run the same long gate
# again. No `timeout` the caller chooses can move that ceiling, because the ceiling is the
# tool's and fires first.
#
# So the wait is split. The gate runs detached, writing its output and then its exit code to
# a state directory; every call here waits a BOUNDED slice of it and returns something the
# caller can act on. Three short calls beat one that is silently truncated, and the caller
# always holds either a verdict or the knowledge that there is not one yet.
#
# THE VERDICT IS KEYED TO WHAT WAS JUDGED — the branch's commit and the ref it lands on,
# both resolved to object ids. A cached pass belongs to that pair and to nothing else: a
# rebase, a new commit, or the base moving underneath all produce a different pair and start
# a new run. Without that the second call after a rebase would hand back the verdict for the
# tree before it, which is the failure this whole file is downstream of — a check answering
# confidently about the wrong thing.
#
# IT FAILS CLOSED, everywhere it can be uncertain. A run whose process is gone without
# having recorded an exit code is reported as a failure, not as a pass and not as "still
# running": those are the two readings that would let unverified work land, and a gate that
# reports its own collapse as all-clear is worse than no gate
# (law-absence-needs-a-positive-control).
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# HOW LONG ONE CALL MAY BLOCK, in seconds. It must stay comfortably under the tool ceiling it
# exists to respect — the whole mechanism is defeated by a slice that is itself backgrounded.
# The margin is for the caller's own overhead and for a slow first tick.
POLL="${SPIRA_GATE_POLL:-480}"
TICK="${SPIRA_GATE_TICK:-3}"

MODE=wait
case "${1:-}" in
    --status) MODE=status; shift ;;
    --exec)   MODE=exec;   shift ;;
    --*)      echo "gate-run: unknown option $1" >&2; exit 1 ;;
esac

BR="${1:?usage: gate-run.sh [--status] <branch> [repo-name]}"
REPO_NAME="${2:-$(spira_home_repo)}"
REPO="$(repo_root "$REPO_NAME")" || {
    echo "gate-run: repo-map has no entry for '$REPO_NAME' — refusing to guess a checkout" >&2
    exit 1; }

# One state directory per branch per repository, named from both because a branch name is
# only unique within its own checkout. Every character outside a safe set is folded, so a
# branch with a slash in it — which every branch here has — does not become a directory tree.
slug="$(printf '%s.%s' "$REPO_NAME" "$BR" | tr -c 'A-Za-z0-9._-' '_')"
D="$SPIRA_RUN/gate-run/$slug"
PIDF="$D/pid"; RCF="$D/rc"; OUT="$D/out"; KEYF="$D/key"; STARTF="$D/started"
SUITESF="$D/suites"

# ---------------------------------------------------------------------------------------
# --exec — the detached run. It records its own pid FIRST, so a parent that returns before
# the fork has settled still finds a live run rather than concluding there is none; and it
# records the exit code LAST and by rename, so a reader never sees a verdict before the
# output it is a verdict about, and never a half-written one.
# ---------------------------------------------------------------------------------------
if [ "$MODE" = exec ]; then
    mkdir -p "$D"
    echo $$ > "$PIDF"
    bash "$(dirname "$0")/gate.sh" "$BR" "$REPO_NAME" > "$OUT" 2>&1
    rc=$?
    # WRITTEN BEFORE THE EXIT CODE, same rule as the exit code itself: a reader must never
    # see a verdict before the scope it was about. gate.sh prints this line on every PASS —
    # fresh or cached — and on nothing else, so a FAIL leaves "-" here rather than a stale
    # scope from whatever this branch last passed.
    _suites="$(grep -m1 '^gate: gate PASS covered suites: ' "$OUT" 2>/dev/null)"
    _suites="${_suites#gate: gate PASS covered suites: }"
    printf '%s\n' "${_suites:--}" > "$SUITESF.part" 2>/dev/null \
        && mv -f "$SUITESF.part" "$SUITESF" 2>/dev/null
    printf '%s\n' "$rc" > "$RCF.part" && mv -f "$RCF.part" "$RCF"
    exit "$rc"
fi

# THE KEY IS RESOLVED BEFORE ANYTHING IS READ, and a branch that will not resolve is fatal
# rather than defaulted. Asking for a gate on a ref that does not exist is a mistake worth a
# message; answering it from a state directory left by an earlier ref of the same name is
# not.
key="$(git -C "$REPO" rev-parse --verify -q "$BR^{commit}" 2>/dev/null)" || key=""
[ -n "$key" ] || { echo "gate-run: $REPO_NAME has no branch '$BR'" >&2; exit 1; }
if base="$(spira_landref "$REPO")"; then
    key="$key $(git -C "$REPO" rev-parse --verify -q "$base^{commit}" 2>/dev/null || echo '-')"
else
    # A repository whose land ref cannot be resolved is gate.sh's refusal to make, not this
    # file's — but the key must still say that the base was unknown, or two runs made under
    # two different unknowns would share a verdict.
    key="$key -"
fi

alive() {               # alive -> 0 if the recorded run is a live gate of ours
    local pid cmd
    [ -f "$PIDF" ] || return 1
    pid="$(cat "$PIDF" 2>/dev/null)"; [ -n "${pid:-}" ] || return 1
    [ -d "/proc/$pid" ] || return 1
    # argv decides, never a pattern over every process on the box: a pattern is a substring
    # of any command line that mentions it, including this one's. Captured and then matched,
    # because `tr | grep -q` under pipefail returns 141 when grep closes the pipe on the
    # first match — so the LIVE case is the one that would read as dead
    # (law-no-grep-q-under-pipefail).
    cmd="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)"
    grep -qF 'gate-run.sh' <<< "$cmd" || return 1
    return 0
}

stop_run() {            # stop the recorded run and forget it
    local pid pgrp
    if [ -f "$PIDF" ] && pid="$(cat "$PIDF" 2>/dev/null)" && [ -n "${pid:-}" ]; then
        # THE PROCESS GROUP, because the run holds a `timeout` and a gate below it, and
        # killing only the leader leaves the suites running against a shared worktree the
        # next run is about to check out from under them. setsid made it the group's leader
        # precisely so this is one signal — but only if setsid was there, so ask /proc
        # whether it really leads a group. Signalling `-$pid` when it does not names some
        # other group entirely, and on a box where one process's pid is another's group id
        # that is a stranger's processes.
        pgrp="$(awk '{print $5}' "/proc/$pid/stat" 2>/dev/null)"
        if [ "${pgrp:-}" = "$pid" ]; then kill -TERM "-$pid" 2>/dev/null; fi
        kill -TERM "$pid" 2>/dev/null
    fi
    rm -rf "$D"
}

# An unmanaged gate — one an agent started in its own foreground and had backgrounded out
# from under it — is still a gate holding a verdict nobody has. It leaves no state directory,
# so the only witness is the process table, read through /proc on argv rather than by a
# pattern search. This is what makes the caller's exit check bind the mistake it is aimed at
# instead of only the well-behaved path (law-guard-binds-the-caller).
unmanaged_gate() {      # unmanaged_gate -> 0 and prints the pid, if one is running
    local p cmd
    for p in /proc/[0-9]*; do
        p="${p#/proc/}"
        [ "$p" = "$$" ] && continue
        # { ...; } 2>/dev/null rather than a redirect on tr alone: when a process exits
        # between the glob and the read, bash itself prints "No such file or directory"
        # to stderr — the redirect on tr only silences tr's own errors, not the shell's
        # failed redirection. This polluted --status output and, since the snapshot is a
        # KEY=value file the pane sources, the stray text broke the parse.
        { cmd="$(tr '\0' ' ' < "/proc/$p/cmdline")"; } 2>/dev/null || continue
        [ -n "$cmd" ] || continue
        grep -qF 'gate.sh' <<< "$cmd" || continue
        grep -qF " $BR" <<< "$cmd" || continue
        printf '%s' "$p"; return 0
    done
    return 1
}

# Zero rather than a number counted from the epoch: the start marker can be missing on a
# state directory that was interrupted mid-creation, and "1789000000s" in a message about a
# gate is the kind of absurd figure a reader spends minutes on.
elapsed() { local s; s="$(cat "$STARTF" 2>/dev/null)"; [ -n "${s:-}" ] || { echo 0; return; }
            echo $(( $(date +%s) - s )); }

report() {              # report -> prints the verdict and exits with it
    local rc
    rc="$(cat "$RCF" 2>/dev/null)"
    case "${rc:-}" in
        0)  echo "gate-run: PASSED $BR in $REPO_NAME after $(elapsed)s"
            echo "gate-run: key $key"
            echo "gate-run: gate PASS covered suites: $(cat "$SUITESF" 2>/dev/null || echo -)"
            tail -5 "$OUT" 2>/dev/null
            exit 0 ;;
        [0-9]*)
            echo "gate-run: FAILED $BR in $REPO_NAME after $(elapsed)s (gate.sh exit $rc)"
            echo "gate-run: key $key"
            cat "$OUT" 2>/dev/null
            exit 1 ;;
    esac
    # No exit code and nothing running. The run died — killed with the session that started
    # it, out of memory, or the box rebooted — and the one thing that must not happen is for
    # that to read as a pass. The state is cleared so the next call starts a run rather than
    # rereading this corpse; a status probe clears nothing, because it is asked from an exit
    # path and a probe that changed the state would change the answer it was asked for.
    echo "gate-run: the gate run for $BR died after $(elapsed)s without recording a verdict" >&2
    tail -20 "$OUT" 2>/dev/null >&2
    [ "$MODE" = wait ] && rm -rf "$D"
    exit 1
}

stale_key() { [ "$(cat "$KEYF" 2>/dev/null)" != "$key" ]; }

# ---------------------------------------------------------------------------------------
# --status — answer now, change nothing. This is what an exit path asks before it records a
# verdict about work whose gate may still be deciding, so every uncertain case answers
# "something is still in flight" rather than "all clear".
# ---------------------------------------------------------------------------------------
if [ "$MODE" = status ]; then
    if alive; then
        # A live run for an EARLIER commit is still a live run. The question here is whether
        # a background task is holding a gate verdict for this branch, and it is — for the
        # wrong tree, which is a reason to say so rather than a reason to say no.
        stale=""; stale_key && stale=" (started for an earlier commit)"
        echo "gate-run: still running for $BR$stale — $(elapsed)s so far, pid $(cat "$PIDF" 2>/dev/null)"
        echo "gate-run: key $key"
        exit 2
    fi
    if [ -d "$D" ]; then
        # A finished run about a different tree answers nothing about this one.
        if [ -f "$RCF" ] && stale_key; then
            echo "gate-run: the recorded run for $BR is for another (tip, base) pair — key $key" >&2
            exit 3
        fi
        report
    fi
    if p="$(unmanaged_gate)"; then
        echo "gate-run: a gate.sh for $BR is running outside this runner, pid $p — its verdict reaches nobody"
        echo "gate-run: key $key"
        exit 2
    fi
    echo "gate-run: no gate has run or finished for $BR — key $key" >&2
    exit 3
fi

# ---------------------------------------------------------------------------------------
# wait — start a run if there is none, then block for a bounded slice of it.
# ---------------------------------------------------------------------------------------
# A run about a different commit, or a different base, is about a different question. Stop it
# and start again rather than waiting on it or, worse, handing back its answer.
if [ -d "$D" ] && stale_key; then
    echo "gate-run: the recorded run is for another commit — starting a new one" >&2
    stop_run
fi

if [ -f "$RCF" ]; then report; fi

if ! alive; then
    if [ -d "$D" ]; then
        # State with no live process and no exit code: an earlier slice's run did not
        # survive. Say so and start over — silently restarting would hide a gate that dies
        # every time behind a caller that patiently calls again.
        echo "gate-run: the previous run for $BR did not finish and is gone — starting a new one" >&2
        stop_run
    fi
    mkdir -p "$D"
    printf '%s' "$key" > "$KEYF"
    date +%s > "$STARTF"
    : > "$OUT"
    # DETACHED, WITH EVERY STREAM ON A FILE. The point is not merely to background it: a
    # child still holding the caller's stdout keeps that call alive, which is the ceiling
    # this file exists to get out from under. setsid also makes it a process group leader,
    # which is what lets stop_run take the whole run down in one signal.
    if command -v setsid >/dev/null 2>&1; then
        setsid bash "$0" --exec "$BR" "$REPO_NAME" </dev/null >/dev/null 2>&1 &
    else
        bash "$0" --exec "$BR" "$REPO_NAME" </dev/null >/dev/null 2>&1 &
    fi
    # The run records its own pid; wait briefly for it to appear rather than trusting $!,
    # which is the wrapper's when setsid forks and the run's when it does not.
    for _ in 1 2 3 4 5 6 7 8 9 10; do alive && break; [ -f "$RCF" ] && break; sleep 0.5; done
    echo "gate-run: started the gate for $BR in $REPO_NAME — this call waits up to ${POLL}s" >&2
fi

deadline=$(( $(date +%s) + POLL ))
while [ ! -f "$RCF" ]; do
    [ "$(date +%s)" -ge "$deadline" ] && break
    alive || break
    sleep "$TICK"
done

[ -f "$RCF" ] && report
alive || report          # gone without a verdict: report() fails closed on it

echo "gate-run: still running for $BR after $(elapsed)s — run the same command again"
echo "gate-run: each call waits up to ${POLL}s; do not end your turn while this is unfinished"
exit 2
