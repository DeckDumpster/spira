#!/usr/bin/env bash
#
# aeon.sh — summon one aeon from a fayth: claim a bead, work it, close or fail it, exit.
#
#   aeon.sh <fayth>            summon one aeon (no-op if at concurrency cap or no work)
#   aeon.sh <fayth> --dry-run  show what would be claimed, claim nothing
#
# WHY STATELESS AND SHORT-LIVED
# -----------------------------
# Gas Town's agents are long-lived tmux sessions holding mailboxes, and a session that
# dies takes its work with it — nothing else knows what it held. An aeon holds a LEASE
# instead: if it dies, the lease goes stale and `bd reclaim` returns the bead to ready.
# Crash recovery becomes a property of the substrate rather than of the agent.
#
# WHY `bd ready --claim` AND NOT SELECT-THEN-CLAIM
# ------------------------------------------------
# Selecting and then claiming is a race with every other aeon. `--claim` is atomic and is
# the documented primitive; hand-rolling it would be the "read the manual first" mistake.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# WHEN THIS AEON STARTED, read once and here rather than wherever it is next wanted. A
# persona with a wall (FAYTH_TIMEOUT_SECONDS) is killed a fixed number of seconds after the
# unit starts, so the deadline is anchored to this line and not to the moment the model is
# launched — by then the claim, the worktree and the fixture have already spent some of it.
AEON_T0="$(date +%s)"

FAYTH="${1:-}"; [ -n "$FAYTH" ] || die "usage: aeon.sh <fayth> [--dry-run | --sweep [--prompt <text>|-]]"
DRY=0; SWEEP=0; SWEEP_PROMPT=""
case "${2:-}" in
    --dry-run) DRY=1 ;;
    --sweep)
        SWEEP=1
        case "${3:-}" in
            --prompt)
                # --prompt - reads stdin; --prompt <text> uses the literal value.
                if [ "${4:-}" = "-" ]; then SWEEP_PROMPT="$(cat)"
                else                         SWEEP_PROMPT="${4:-}"
                fi
                ;;
            -)  SWEEP_PROMPT="$(cat)" ;;   # bare - is a stdin shorthand
        esac
        ;;
esac
F="$SPIRA_HOME/chamber/$FAYTH.fayth"
[ -f "$F" ] || die "no such fayth: $F"
# shellcheck disable=SC1090
. "$F"

# SPIRA_REQUIRE_LABEL — set by summon_fayth when the SLOT itself is restricted (an express
# grant under the admission throttle, sp-zcvh1). Folded into FAYTH_LABELS before the fence
# and the claim below, so both see it: a bead that does not carry it is not this aeon's to
# take, no matter how the fayth's own predicate reads.
[ -n "${SPIRA_REQUIRE_LABEL:-}" ] && FAYTH_LABELS="${FAYTH_LABELS:+$FAYTH_LABELS,}$SPIRA_REQUIRE_LABEL"

# ---- the fence -----------------------------------------------------------------------
# Bound to the actor that would do the damage. An installation that imported a predecessor's
# beads has a ready queue full of work that predecessor is still doing, and the only thing
# that has ever kept an aeon off them is a config string in a fayth being right. A predicate
# that omits `spira` claims somebody else's work, so refuse to claim at all rather than trust
# the string.
fayth_fenced "$FAYTH" "${FAYTH_LABELS:-}" || die "$FAYTH: refusing to claim behind an unfenced predicate"

# ---- the ledger ----------------------------------------------------------------------
# Two lines per aeon, and the GAP BETWEEN THEM is the measurement. `born` is written within
# milliseconds of exec; `awake` is written once the claim has resolved, which is the first
# thing here that takes real time.
#
# The first aeon this harness ever summoned was killed inside the same second: the sentinel
# service is Type=oneshot with the default KillMode=control-group, so systemd tore down the
# whole cgroup the moment the pass finished, after 1.6s of CPU. The sentinel went on
# reporting "summoned" every two minutes into an empty log, and nothing anywhere counted the
# difference between a summon and a worker. Born-without-awake is that failure and no other,
# which is what makes it a number instead of a story. The cockpit reads it.
#
# Written HERE rather than at the summoning site because aeons arrive from two places — the
# sentinel's CHECK 7 and spira-ops.service — and a ledger kept by one of them would report
# the other's aeons as never having existed.
#
# Append-only, one short line, no locking: O_APPEND is atomic for a write this size, and the
# alternative is an aeon that cannot start because a lock outlived a killed one.
LEDGER="$SPIRA_RUN/aeon-ledger.log"
# A DRY RUN INSPECTS; IT DOES NOT SUMMON. The muzzle lives in the writer rather than at each
# call site because the capacity check sits above the --dry-run branch: an inspection run
# while an aeon is working exits at capacity, so guarding only the birth left a disposition
# with no birth behind it — the born/awake ledger with its two halves swapped, written by
# the one command a human types by hand.
ledger() {
    [ "$DRY" = 1 ] && return 0
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >> "$LEDGER"
}

# rapid_recur_check — when the last SPIRA_RAPID_RECUR_THRESHOLD done lines for BEAD_ID
# all show wall_s=? or wall_s<10, each summon is dying before doing real work: a setup loop
# that recurs identically on every retry (law-a-retry-must-change-an-input), so a fourth
# summon cannot learn anything the third did not. Park with $SPIRA_ASK_LABEL rather than
# only annotating — an annotation left dispatch free to keep re-summoning into the same
# fault, re-appending the same note forever.
rapid_recur_check() {
    local _threshold="${SPIRA_RAPID_RECUR_THRESHOLD:-3}"
    [ -n "${BEAD_ID:-}" ] || return 0
    [ -f "${LEDGER:-}" ] || return 0
    local _count
    _count=$(grep " done [^ ]* $BEAD_ID " "$LEDGER" | tail -"$_threshold" | python3 -c '
import sys, re
count = 0
for line in sys.stdin:
    m = re.search(r"wall_s=(\?|[0-9.]+)", line)
    if m and (m.group(1) == "?" or float(m.group(1)) < 10.0):
        count += 1
    else:
        count = 0
print(count)' 2>/dev/null) || return 0
    [ "${_count:-0}" -ge "$_threshold" ] || return 0
    # Idempotent: once parked, a bead carrying SPIRA_ASK_LABEL is excluded from every fayth
    # predicate, so it should not be summoned again — this also guards against re-noting if
    # it somehow is.
    case "$(bdq label list "$BEAD_ID" 2>/dev/null)" in *"${SPIRA_ASK_LABEL}"*) return 0 ;; esac
    log "$FAYTH: $BEAD_ID RAPID-RECUR: $_count consecutive sub-10s runs — parking, a setup loop cannot be learned from a retry"
    bdq label add "$BEAD_ID" "$SPIRA_ASK_LABEL" >/dev/null 2>&1 || true
    bdq label add "$BEAD_ID" "overseer" >/dev/null 2>&1 || true
    bdq note "$BEAD_ID" \
        "RAPID-RECUR: $_count consecutive sub-10s aeon runs on $BEAD_ID. Each summon dies before meaningful work, suggesting a setup loop — the defect recurs identically on every retry. Parked with $SPIRA_ASK_LABEL and overseer instead of only annotated: a fourth summon cannot learn anything the third did not. Check: worktree path, conflicting branches, or box state. Details in aeon-ledger." \
        >/dev/null 2>&1 || true
    spira_event aeon.rapid "$BEAD_ID" \
        "Rapid-recur: $BEAD_ID — $_count consecutive sub-10s aeon summons (setup loop) — parked" || true
}

# ledger_done <rc> <status> — an aeon's disposition line, with what its session SPENT.
#
# THE SPEND IS ON THIS LINE BECAUSE NOTHING ELSE KEEPS IT. The client writes duration, turns,
# tokens and cost to the terminal `result` record of every session and that trace is the only
# copy; reading it back over a corpus of them is a purpose-built script, and reading it here
# is one field of one short line. session_result_fields (lib.sh) is the parser, and the
# figures are the SESSION's — a fayth's aeons can be compared with each other, and cost per
# landed bead is an awk one-liner over this file.
#
# EVERY DISPOSITION CARRIES THE FIELDS, including the ones written before a session could
# have run. A reader that must first know which statuses have them is a reader that will get
# it wrong, and the ones without a session render `?` — never 0, which would say the session
# ran and cost nothing.
ledger_done() {
    ledger "done $FAYTH $BEAD_ID rc=$1 status=$2 $(session_result_fields "${LOGF:-}")"
    rapid_recur_check || true
}

# Bounded here rather than by logrotate: this file is read in full on every cockpit pass,
# and an unbounded input to something that runs every minute is a slow leak with a deadline.
# The -f test is not redundant with wc's own error: `< "$LEDGER"` is the SHELL's redirection
# and it reports a missing file on the shell's stderr, which wc's 2>/dev/null cannot reach.
[ -f "$LEDGER" ] && [ "$(wc -l < "$LEDGER")" -gt 20000 ] \
    && { tail -n 5000 "$LEDGER" > "$LEDGER.trim" && mv -f "$LEDGER.trim" "$LEDGER"; }
ledger "born $FAYTH $$"

# aeon_settings and aeon_claude_argv (the claude CLI argv both launch sites share) live in
# lib.sh next to summon_argv (the systemd-run argv summon_fayth and escape.sh share) — one
# seam per shared invocation, testable without running this script's own claim/work/close body.

# ---- sweep mode: a beadless session --------------------------------------------------
# A SWEEP RUNS THE PERSONA WITHOUT A BEAD. The bead lifecycle — claim, lease, close,
# verdict, attempt — does not apply. What does apply is the capacity check, the draining
# check, the concurrency cap, and the born/awake/done ledger lines. Those are shared
# because a sweep spends the same account window a builder does and must appear in the
# cockpit's aeon counts — a stillborn sweep must show as born-without-awake just as a
# stillborn builder does.
#
# THE FENCE IS CLAIM-SPECIFIC. fayth_fenced refuses a predicate that does not filter to
# this installation's own beads — it exists to stop a persona claiming somebody else's
# work. A sweep does not claim, so the fence has nothing to guard and must not run here.
# It is not deleted for that reason; claim mode reaches it on the path below.
if [ "$SWEEP" = 1 ]; then
    have_sw="$(aeon_count "$FAYTH")"
    if [ "$have_sw" -ge "${FAYTH_MAX_CONCURRENT:-1}" ]; then
        log "$FAYTH: at capacity ($have_sw/${FAYTH_MAX_CONCURRENT:-1}), not sweeping"
        ledger "awake $FAYTH capacity"
        exit 0
    fi
    if [ -f "${SPIRA_RUN:-}/world.draining" ]; then
        log "$FAYTH: draining — not sweeping (world.sh resume to lift)"
        ledger "awake $FAYTH draining"
        exit 0
    fi
    if capacity_paused; then
        log "$FAYTH: the account is out of capacity for another ${SPIRA_CAPACITY_LEFT}s — not sweeping"
        ledger "awake $FAYTH paused"
        exit 0
    fi

    # Identity: take a name from the pool so the pane can distinguish concurrent sweeps.
    SWEEP_AEON="$(aeon_name_take "$FAYTH")"
    export SPIRA_AEON="$SWEEP_AEON"
    export BEADS_ACTOR="aeon-$SWEEP_AEON"
    export GIT_AUTHOR_NAME="aeon-$SWEEP_AEON" GIT_AUTHOR_EMAIL="aeon-$SWEEP_AEON@spira.local"
    export GIT_COMMITTER_NAME="aeon-$SWEEP_AEON" GIT_COMMITTER_EMAIL="aeon-$SWEEP_AEON@spira.local"

    # Pidfile keeps this sweep visible to aeon_count for the duration of the session.
    # Named with $$ to avoid collisions with concurrent sweeps or bead workers.
    SWEEP_PIDFILE="$SPIRA_RUN/aeon-$FAYTH-sweep-$$.pid"
    printf '%s' "$SWEEP_AEON" > "${SWEEP_PIDFILE%.pid}.name"
    echo $$ > "$SWEEP_PIDFILE"

    # BEAD_ID placeholder for ledger lines: the literal string "sweep".
    # Readers that parse the ledger for born/awake/done counts see a distinguishable token
    # rather than an empty field; the cockpit's aeon_count reads pidfiles, not this value.
    SWEEP_BEAD="sweep"
    ledger "awake $FAYTH $SWEEP_BEAD"

    SWEEP_LOGF="$SPIRA_RUN/sweep-$FAYTH-$$.log"
    log "$FAYTH: sweeping (log: $SWEEP_LOGF)"

    # Teardown: release the pidfile and write the disposition line regardless of exit path.
    sweep_cleanup() {
        local rc=$?
        set +e
        rm -f "$SWEEP_PIDFILE" "${SWEEP_PIDFILE%.pid}.name"
        ledger "done $FAYTH $SWEEP_BEAD rc=$rc status=sweep $(session_result_fields "${SWEEP_LOGF:-}")"
        # SAME RATIONALE AS THE BEAD-MODE TEARDOWN: a sweep that ran and did work
        # succeeded, even if the claude CLI exited 1 because a tool call returned non-zero.
        # Exit 0 only when the session actually ran (unlanded or killed mid-work); a
        # refused session (API capacity) preserves $rc so the named unit enters FAILED.
        case "$(session_outcome "${SWEEP_LOGF:-}" 2>/dev/null)" in
            unlanded|killed) exit 0 ;;
        esac
        exit $rc
    }
    trap sweep_cleanup EXIT INT TERM

    # Prompt: caller-supplied text (from --prompt or -) prepended with statutes. If no
    # prompt was given, use the fayth's .md as-is — without bead substitutions, since
    # there is no bead. The fayth .md is still useful as the persona's standing brief.
    SWEEP_STATUTES="$(render_memories "${FAYTH_MEMORY_PREFIXES:-law-}" "" "${FAYTH_STATUTE_CORE:-}")"
    if [ -z "$SWEEP_PROMPT" ]; then
        SWEEP_PROMPT="$(cat "$SPIRA_HOME/chamber/$FAYTH.md" 2>/dev/null || true)"
    fi
    SWEEP_SYSTEM_FILE="$SPIRA_RUN/sweep-$FAYTH-$$.system.md"
    SWEEP_TASK_FILE="$SPIRA_RUN/sweep-$FAYTH-$$.task.md"
    system_prompt_split "$SWEEP_SYSTEM_FILE" "$SWEEP_TASK_FILE" "$SWEEP_STATUTES" "$SWEEP_PROMPT"

    mapfile -t _SWEEP_ARGV < <(aeon_claude_argv "$SPIRA_SYSTEM_FLAG" "$SWEEP_SYSTEM_FILE")
    set +e
    cat "$SWEEP_TASK_FILE" | \
        ${FAYTH_TIMEOUT_SECONDS:+timeout $FAYTH_TIMEOUT_SECONDS} \
        "${SPIRA_AGENT:-claude}" "${_SWEEP_ARGV[@]}" \
        >> "$SWEEP_LOGF" 2>&1
    exit $?
fi
# ---- end sweep mode ------------------------------------------------------------------

# ---- concurrency ---------------------------------------------------------------------
# THE SAME CHOKEPOINT fayth_free GUARDS FOR THE SENTINEL (lib.sh), not a second copy of its
# rule: this compared `have` to FAYTH_MAX_CONCURRENT alone and never saw FAYTH_ELASTIC, so an
# elastic persona was capped at its own fallback number no matter how large the declared pool
# was (sp-4gxjo) — the sentinel kept summoning past it because CHECK 7 already asked
# fayth_free the right question. Passing SPIRA_MAX_AEONS through as the remainder is the same
# subtraction sentinel.sh does before every fayth_free call; a non-elastic persona gets an
# empty pool argument and fayth_free falls through to FAYTH_MAX_CONCURRENT exactly as before.
have="$(aeon_count "$FAYTH")"
limit="${FAYTH_MAX_CONCURRENT:-1}"
pool=""
if [ "$(fayth_get "$FAYTH" FAYTH_ELASTIC 0)" = 1 ] && [ -n "${SPIRA_MAX_AEONS:-}" ]; then
    limit="$SPIRA_MAX_AEONS"
    pool=$(( SPIRA_MAX_AEONS > have ? SPIRA_MAX_AEONS - have : 0 ))
fi
if [ "$(fayth_free "$FAYTH" "$pool")" -eq 0 ]; then
    log "$FAYTH: at capacity ($have/$limit), not summoning"
    # A healthy no-op, and it must read as one: an aeon that declined to summon LIVED, it
    # simply had nothing to do. Counting it as stillborn would put a permanent false
    # reading on the panel every time the harness was correctly at capacity.
    ledger "awake $FAYTH capacity"
    exit 0
fi

# ---- halted ---------------------------------------------------------------------------
# Same reasoning as draining below — aeons launched directly by ExecStart bypass
# summon_fayth, so this guard must live here too (law-guard-binds-the-caller).
if [ -f "${SPIRA_RUN:-}/world.halted" ]; then
    log "$FAYTH: halted — claiming nothing (world.sh start to lift)"
    ledger "awake $FAYTH halted"
    exit 0
fi

# ---- draining -------------------------------------------------------------------------
# BOUND HERE FOR THE REASON THE PARAGRAPH BELOW ALREADY GIVES, and it is here because that
# paragraph was not read closely enough the first time. The drain gate went into
# summon_fayth() alone, on the strength of a grep showing summon_fayth is called only from
# sentinel.sh. That grep was true and the conclusion was wrong: spira-ops.service and
# spira-qa.service ExecStart THIS SCRIPT directly, so ops and qa never pass through
# summon_fayth at all. A qa aeon was summoned four minutes into a drain that reported
# DRAINED (sp-637b, 2026-09-08 20:06). law-guard-binds-the-caller, in the one shape the
# file already warned about.
#
# Exit 0, not 1: an aeon that correctly declined LIVED, exactly as the capacity and
# concurrency cases below argue. A failed unit here would make a deliberate drain look like
# a broken timer every time it fired.
if [ -f "${SPIRA_RUN:-}/world.draining" ]; then
    log "$FAYTH: draining — claiming nothing (world.sh resume to lift)"
    ledger "awake $FAYTH draining"
    exit 0
fi

# ---- the account ----------------------------------------------------------------------
# BOUND HERE AS WELL AS AT summon_fayth, because aeons arrive from two places — the
# sentinel's CHECK 7 and spira-ops.service — and a guard on one of them binds whichever
# caller is most disciplined about using it rather than the one that does the damage
# (law-guard-binds-the-caller). Both doors reach this line; the ready queue is behind it.
#
# Checked BEFORE the claim, never after: the whole point is that no bead is holding a lease
# while the window is shut, so there is nothing to hand back and nothing to charge.
if capacity_paused; then
    log "$FAYTH: the account is out of capacity for another ${SPIRA_CAPACITY_LEFT}s — claiming nothing"
    # LIVED, did not work. Same reading as the concurrency cap above and for the same
    # reason: an aeon that correctly declined is not a stillbirth, and counting it as one
    # would put a false number on the panel for the whole of every outage.
    ledger "awake $FAYTH paused"
    exit 0
fi

# ---- claim ---------------------------------------------------------------------------
# READY_ARGS IS THE SENTINEL'S OWN QUERY (lib.sh), not a copy of it. CHECK 7 summons on a
# count and this claims out of that count, so the day the two definitions drift is the day
# an aeon is summoned every two minutes for work it cannot take. That is what each of the
# three flags in READY_ARGS is there to prevent, and it happened when only some of them
# were written here.
# THE EXCLUSIONS ARE COMPUTED, NOT READ. A bead may name the persona it wants with a
# `fayth:<name>` label, so this persona must not claim one that named somebody else — and
# the count CHECK 7 summoned on was computed the same way. The two definitions drifting is
# the day an aeon is summoned every two minutes for work it cannot take, which is what
# READY_ARGS is shared to prevent; fayth_exclude is shared for the same reason.
CLAIM_EXCLUDE="$(fayth_exclude "$FAYTH" "$FAYTH_EXCLUDE_LABELS")"
claim_args=("${READY_ARGS[@]}" --claim
            --label "$FAYTH_LABELS" --exclude-label "$CLAIM_EXCLUDE")

if [ "$DRY" = 1 ]; then
    log "$FAYTH: dry run — candidates:"
    bdq "${READY_ARGS[@]}" --label "$FAYTH_LABELS" \
        --exclude-label "$CLAIM_EXCLUDE" 2>/dev/null | grep -vE '^💡|^warning|^  Fix|^  Or' | head -10
    exit 0
fi

# THIS AEON'S NAME. Held for the life of the session and written beside its pidfile, so
# the pane, `bd` history and the commit graph all name the same instance.
AEON="$(aeon_name_take "$FAYTH")"
export SPIRA_AEON="$AEON"
export BEADS_ACTOR="aeon-$AEON"
export GIT_AUTHOR_NAME="aeon-$AEON" GIT_AUTHOR_EMAIL="aeon-$AEON@spira.local"
export GIT_COMMITTER_NAME="aeon-$AEON" GIT_COMMITTER_EMAIL="aeon-$AEON@spira.local"
# RESUMPTION BEATS INITIATION — WITHIN ONE PRIORITY, NEVER ACROSS ONE. `bd ready --claim`
# takes the first row, and priority was the only ordering — so a bead carrying 21 commits
# and an open pull request lost to a bead with nothing started, twice. That is not untidy,
# it is expensive: an unfinished branch decays, its base moves under it, and every pass it
# sits costs another rebase.
#
# So look for resumable work FIRST: a ready bead whose recorded branch exists and is ahead
# of its base. Claim that one by id, atomically, with `bd update --claim`. Only when there
# is none do we fall back to taking the head of the queue.
#
# THE PRIORITY FLOOR IS THE HALF THAT WAS MISSING, and without it this block was a priority
# inversion that starved every P0 in the queue. The loop took the first RESUMABLE candidate
# at any depth, so one P1 with a single commit on its branch beat seven P0s with nothing
# started — measured 2026-09-07: the queue's head was sp-2tv (P0, the bead describing this
# very starvation) and the loop reached past it to candidate twelve, sp-4vp (P1, one commit
# ahead), on every pass. The operator watched more than five aeons walk over it.
#
# A resumable bead is worth preferring over an unstarted PEER. It is not worth preferring
# over more important work: the decaying-branch cost this block exists to avoid is bounded
# by a rebase, while the cost of never starting a P0 is unbounded. So candidates are
# filtered to the best priority present before the branch test runs, and a lower band is
# reached only when the whole band above it is unstarted — which is exactly when the
# fallback `bd ready --claim` head is already the right bead.
#
# Filtered in python over the whole payload rather than by breaking out of the loop on the
# first priority change, so it does not silently depend on `bd ready` returning rows in
# priority order — an ordering nothing promises and one this file already learned not to
# trust for the claim itself.
# EVERY RESUMABLE CANDIDATE IN THE TOP BAND IS KEPT, not just the first: aeons summoned
# seconds apart run this same query and reach the same first candidate, so a fallback that
# gives up the instant that one claim is lost sends the loser straight to the general claim
# — which may or may not pick a different bead — instead of the NEXT resumable one it already
# knows about (sp-3ntca). One `git rev-list` per candidate in the band, which is the same
# work the old break-on-first form paid up to its one candidate.
resume_ids=()
for cand in $(bdjson "${READY_ARGS[@]}" --label "$FAYTH_LABELS" \
                  --exclude-label "$FAYTH_EXCLUDE_LABELS" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
rows = d if isinstance(d, list) else [d]
# A row with no priority sorts last, not first: an unknown must never outrank a stated P0.
def prio(i):
    p = i.get("priority")
    return p if isinstance(p, int) else 99
if not rows: sys.exit(0)
top = min(prio(i) for i in rows)
for i in rows:
    if prio(i) != top: continue
    labs = i.get("labels") or []
    br = next((l[7:] for l in labs if l.startswith("branch:")), "")
    repo = next((l[5:] for l in labs if l.startswith("repo:")), "")
    print("%s|%s|%s|%s" % (i["id"], br, repo, top))' 2>/dev/null); do
    cid="${cand%%|*}"; rest="${cand#*|}"; cbr="${rest%%|*}"
    rest="${rest#*|}"; crepo="${rest%%|*}"; cprio="${rest##*|}"
    [ -n "$cbr" ] || cbr="spira/$cid"
    croot="$(repo_root "${crepo:-}" 2>/dev/null)" || continue
    [ -d "$croot/.git" ] || continue
    cbase="$(spira_landref "$croot")"
    # Ahead of its base is the test — a branch that exists but adds nothing is not
    # resumable work, it is a leftover.
    n="$(git -C "$croot" rev-list --count "$cbase..$cbr" 2>/dev/null || echo 0)"
    if [ "${n:-0}" -gt 0 ]; then resume_ids+=("$cid"); RESUME_PRIO="$cprio"; fi
done

# claim_retry's own diagnostic on a failed attempt goes to ITS stderr, not a variable a
# command substitution would just discard — so every call site here redirects that stderr to
# a scratch file and reads it back, rather than reading a global claim_retry could never have
# set (see claim_retry, lib.sh).
_claim_err="$SPIRA_RUN/.claim-err.$$"

claimed=""
for resume_id in "${resume_ids[@]:-}"; do
    [ -n "$resume_id" ] || continue
    claimed="$(claim_retry update "$resume_id" --claim 2>"$_claim_err")"
    claim_rc=$?
    claim_errmsg="$(cat "$_claim_err" 2>/dev/null)"
    if [ "$claim_rc" -ne 0 ]; then
        log "$FAYTH/$AEON: claim query failed for resume candidate $resume_id: ${claim_errmsg:-bd gave no reason} — trying the next resumable candidate"
        claimed=""
        continue
    fi
    if [ -n "$claimed" ]; then
        log "$FAYTH/$AEON: resuming $resume_id (P${RESUME_PRIO:-?}, the top ready priority) — it already has work on its branch"
        break
    fi
    log "$FAYTH/$AEON: resume candidate $resume_id was claimed by another aeon between read and claim — trying the next resumable candidate"
done

# THE GENERAL CLAIM. Reached when no resume candidate existed or every one of them lost its
# race. A bd ERROR here is not an empty queue — see claim_retry above — so only a query that
# actually completed and returned zero rows may read as idle below.
if [ -z "$claimed" ]; then
    claimed="$(claim_retry "${claim_args[@]}" 2>"$_claim_err")"
    claim_rc=$?
    claim_errmsg="$(cat "$_claim_err" 2>/dev/null)"
    rm -f "$_claim_err"
    if [ "$claim_rc" -ne 0 ]; then
        log "$FAYTH: claim-error ${claim_errmsg:-bd gave no reason} — retries exhausted, not reporting idle for a query that never completed"
        ledger "awake $FAYTH claim-error ${claim_errmsg:-bd gave no reason}"
        exit 1
    fi
else
    rm -f "$_claim_err"
fi
# THE ID AND THE REPOSITORY, FROM THE SAME PAYLOAD. `bd ready --claim` already handed us
# the bead's labels, so asking the database again for the one it just gave us would be a
# second, racier opinion of the same fact.
read -r BEAD_ID BEAD_REPO <<< "$(printf '%s' "$claimed" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d if isinstance(d, list) else [d]
if not d: sys.exit(0)
repo = next((l[5:] for l in (d[0].get("labels") or []) if l.startswith("repo:")), "")
print(d[0]["id"], repo)' 2>/dev/null)"

if [ -z "${BEAD_ID:-}" ]; then
    log "$FAYTH: nothing ready to claim"
    ledger "awake $FAYTH idle"
    exit 0
fi
log "$FAYTH/$AEON: claimed $BEAD_ID"
ledger "awake $FAYTH $BEAD_ID"
# A CLAIM IS A TRANSITION, AND IT IS THE ONE THE PANE COULD NOT SHOW. Every other outcome
# recorded here is an ENDING — landed, reopened, poisoned, reclaimed — so a bead changing
# hands was invisible: an aeon could exit mid-CI believing something would resume it, the
# lease expired, and the health pane showed a stale landing while commits sat abandoned.
# Nothing on screen said a thing had changed hands.
#
# Emitted AFTER the ledger, never instead of it. The ledger is what aeon_count and the
# born/awake positive control read, and it must not depend on a database being reachable.
spira_event aeon.claimed "$BEAD_ID" "$AEON claimed $BEAD_ID" "summoned from the $FAYTH fayth" || true

# THE CLAIM AND THE PREDICATE CHECK ARE NOT ATOMIC. The predicate excludes spira-poison,
# but `bd ready --claim` reads, then writes: a bead that receives the poison label in that
# gap is claimed by an aeon that is excluded from working it. One was claimed one second
# after the label landed and retried sp-m56w identically until the database ran out of
# attempts.
#
# Read-after-claim is the only reliable check: re-read the bead's labels the moment after
# claiming and release if spira-poison is present. The window is short enough that the
# check is virtually free, and the cost of missing it is a retried-identical session.
if bead_has_label "$(bdjson show "$BEAD_ID" 2>/dev/null)" spira-poison; then
    release_own_claim "$BEAD_ID"
    log "$FAYTH/$AEON: $BEAD_ID carries spira-poison — released immediately after claim (race with the label)"
    ledger_done 0 poison-raced
    exit 0
fi

# ---- world-stop fence ----------------------------------------------------------------
# A bead labelled SPIRA_WORLD_STOP_LABEL declares it needs the world halted while it
# runs — world.sh stop must be called before the session starts and world.sh start must
# be called after, whether or not the session succeeds.
#
# WHY THIS FENCE AND NOT ONLY THE FILING GUARD. _bdq_check_destructive (lib.sh) stops a
# bead from being filed without needs-ryan — it fires before the verdict that makes a
# halting bead dispatchable. This fence fires after that verdict: once the operator
# approves the work, the next summon must still drain the live pool before proceeding.
# sp-6ylz had the danger in its title and was still claimed while aeons were running;
# the title is prose, and prose is what nothing reads.
#
# FENCE, NOT SANDBOX. Every guard names its own override so the operator at the keyboard
# can proceed when the situation is understood. SPIRA_WORLD_STOP_SKIP=1 is the override;
# the refusal names it explicitly. A sandbox would refuse with no exit.
#
# OUR OWN PIDFILE IS NOT YET WRITTEN — it is written below, beside the teardown — so
# every pidfile we find here belongs to a peer, not to ourselves.
WORLD_WAS_STOPPED=0
_world_stop_label="$SPIRA_WORLD_STOP_LABEL"
_world_has_label=0
bead_has_label "$(bdjson show "$BEAD_ID" 2>/dev/null)" "$_world_stop_label" && _world_has_label=1
_world_live=""
for _pf in "$SPIRA_RUN"/aeon-*.pid; do
    [ -e "$_pf" ] || continue
    _pid="$(cat "$_pf" 2>/dev/null)" || continue
    [ -n "$_pid" ] && [ -d "/proc/$_pid" ] || { rm -f "$_pf"; continue; }
    _world_live="${_world_live:+$_world_live, }$(basename "$_pf" .pid)"
done
unset _pf _pid
_world_skip=0
[ -n "${SPIRA_WORLD_STOP_SKIP:-}" ] && _world_skip=1
case "$(world_stop_decide "$_world_has_label" "$_world_live" "$_world_skip")" in
refuse)
    # REFUSE, NOT PROCEED. Live aeons write to the database; running world.sh stop
    # under them is what produced the three-minute outage this bead was filed to
    # prevent. The fence releases the claim so the bead goes back to the ready queue,
    # where it will be picked up once the live aeons finish naturally.
    release_own_claim "$BEAD_ID"
    log "$FAYTH/$AEON: $BEAD_ID carries $_world_stop_label — live aeons present ($_world_live) — released. Set SPIRA_WORLD_STOP_SKIP=1 to override."
    bdq note "$BEAD_ID" "Released by aeon.sh: this bead carries $_world_stop_label and requires the world halted while it runs. Live aeons are present ($_world_live) and the world was not stopped. Wait for them to finish, or set SPIRA_WORLD_STOP_SKIP=1 to proceed with live aeons." >/dev/null 2>&1
    ledger_done 0 world-stop-fence  # literal-ok: internal ledger category, not a label predicate
    exit 0
    ;;
stop)
    # No live aeons (or operator override set): stop the world before the session.
    log "$FAYTH/$AEON: $BEAD_ID carries $_world_stop_label — stopping the world before this session${_world_live:+ (SPIRA_WORLD_STOP_SKIP set, live: $_world_live)}"
    "$SPIRA_HOME/world.sh" stop --why "$_world_stop_label bead $BEAD_ID" >/dev/null 2>&1 \
        || log "$FAYTH/$AEON: $BEAD_ID world.sh stop returned non-zero — proceeding"
    WORLD_WAS_STOPPED=1
    ;;
esac
unset _world_stop_label _world_has_label _world_live _world_skip

# ---- the workspace -------------------------------------------------------------------
# THE REPOSITORY COMES FROM THE BEAD. A fayth supplies the persona, the statutes and the
# tool allowlist; the bead supplies the workspace, through the same `repo:<name>` partition
# every imported Gas Town bead carries. FAYTH_REPO was a constant per persona and every
# fayth pointed at the home checkout, so the harness could not touch any other
# repositories whose beads it had just spent a design collapsing into one database.
#
# AND AN UNKNOWN NAME IS REFUSED, never defaulted. Falling back to the home repo would put
# one repository's fix on a branch cut in another, and every downstream check would pass it:
# the aeon committed, the commit names the bead, the gate ran, the branch landed. Nothing
# after this point can tell that the work went to the wrong disk, so it has to stop here.
REPO_NAME="${BEAD_REPO:-$(spira_home_repo)}"
if ! REPO="$(repo_root "$REPO_NAME")" || [ ! -e "$REPO/.git" ]; then
    log "$FAYTH: $BEAD_ID names repo:$REPO_NAME, which repo-map does not resolve to a checkout"
    park_unmapped "$BEAD_ID" "$REPO_NAME"
    ledger_done 1 unmapped-repo
    exit 1
fi
REPO_LAND="$(repo_land "$REPO_NAME")"
# THE INTAKE INHERITS THE REPOSITORY THIS AEON IS WORKING IN. incident.sh needs a repo: label
# and has no way to derive one: an aeon that files an incident mid-session declares nothing,
# so the bead lands repo-less, is worked in the home-repo fallback — which has not held the
# harness since sp-9tal — and the intake escalates the missing label to the operator. The
# aeon is the one party that already knows the answer, because repo-map resolved it above to
# decide which checkout to cut a worktree in. Exported rather than passed at a call site,
# because the caller is a model deciding to file an incident, not a line of this script.
# A caller with a better answer still wins: incident.sh prefers a repo: already in LABELS.
export SPIRA_INCIDENT_REPO="$REPO_NAME"
log "$FAYTH: $BEAD_ID works repo:$REPO_NAME at $REPO (land=$REPO_LAND)"
# THE BRANCH IS A PROPERTY OF THE WORK, RECORDED ON THE BEAD — not a string derived from
# its id (the operator, verbatim: "it seems like there's a needed affinity between bead and
# branch"). Derivation is a convention, and it breaks the moment work outlives the bead
# that began it: sp-pd-ci-green cut its own branch for a deliverable sp-pd-ci-collapse had
# already built, and opened a second pull request for the same nineteen commits.
#
# Recorded, the affinity survives all three cases that matter: an aeon dying and another
# resuming (same bead, same branch); a bead reopened after a failed gate; and work handed
# to a SUCCESSOR bead, which inherits the branch by copying one label rather than starting
# a parallel history.
BRANCH="$(bdq state "$BEAD_ID" branch 2>/dev/null)"
# bd state prints "(no branch state set)" when the dimension has never been written; that
# is not a branch name. Strip the sentinel so the absence test below still works.
case "$BRANCH" in '('*) BRANCH="" ;; esac
if [ -z "$BRANCH" ]; then
    BRANCH="spira/$BEAD_ID"
    bdq set-state "$BEAD_ID" "branch=$BRANCH" >/dev/null 2>&1
    log "$FAYTH/$AEON: $BEAD_ID takes branch $BRANCH"
else
    log "$FAYTH/$AEON: $BEAD_ID resumes recorded branch $BRANCH"
fi
PIDFILE="$SPIRA_RUN/aeon-$FAYTH-$BEAD_ID.pid"
# DEFINED HERE, ABOVE THE HEARTBEAT, because the heartbeat reads it. It used to be assigned
# beside the session that writes it, 150 lines below the subshell that forks with a copy of
# the environment as it stands HERE — so `stat -c %s "$LOGF"` expanded an unbound variable
# under `set -u` on every beat. That does not kill the beat: the error dies inside the
# command substitution, `now` comes back empty, and empty compares equal to the previous
# empty, so the stall counter read "no progress" on a session doing nothing but progress.
# Every aeon therefore stopped heartbeating after STALL_BEATS beats — 20 minutes — however
# hard it was working; its lease expired, strand.sh reclaimed it as a ghost and BUMPED ITS
# ATTEMPT. That is the same harm this bead is about (an attempt spent on something that is
# not the work's fault) arriving through a second door, and it left 94 `line 257: LOGF:
# unbound variable` lines in the user journal in six hours to say so.
#
# ONE FILE PER BEAD, HELD ACROSS ATTEMPTS, and the heartbeat's growth check is why it is one
# file and not one per attempt: it watches this exact path grow, so a name that changed each
# time would leave it staring at a file nobody writes — which reads as a wedged session and
# costs the bead its lease. Every attempt appends a segment behind a mark line; lib.sh's
# attempt_trace is the only thing that knows how to find the newest one.
LOGF="$SPIRA_RUN/$BEAD_ID.log"
echo $$ > "$PIDFILE"
printf '%s' "$AEON" > "${PIDFILE%.pid}.name"

# THE SEGMENT OPENS HERE, ABOVE THE TRAP AND ABOVE THE HEARTBEAT, and not beside the session
# that fills it. Both of those read the log to decide what is happening NOW, and between this
# line and the session there are three ways to leave — an unresolvable base ref and two
# worktree failures — every one of which lands in the teardown below. With no mark of its own
# yet, this attempt's readers would find the PREVIOUS attempt's segment and answer about it:
# a predecessor refused for want of capacity would make a worktree failure read as a refusal,
# so no attempt would be charged and summoning would pause against an epoch from a session
# that is over. Opening the segment first costs a mark line on an attempt that never ran a
# session, which is worth having anyway — it is the record that the attempt happened at all.
spira_trace_mark "$LOGF" "$AEON" >> "$LOGF"

# ---- teardown ------------------------------------------------------------------------
HB_PID=""
FIXTURE_LIB=""
# THE FIXTURE IS DROPPED FIRST, ahead of every branch below — one of them exits on its own,
# and a teardown that returns before reaching its last step is how a database survives the
# process that owned it. It shares a server with live data, so litter there is never noticed
# until it is a problem.
#
# `TESTDB_SHARED=0` is what makes testdb_drop stop being a no-op: a BORROWER must never drop
# a fixture the lender's other readers are still using, so the owner clears the flag to say it
# is the owner. In a subshell, because the drop is the last thing this fixture is for and
# sourcing the library into the teardown of a supervisor buys nothing.
#
# ONLY A FIXTURE THIS PROCESS BUILT. FIXTURE_LIB is set nowhere but the successful build
# below, so it doubles as the record of ownership — and it has to, because TESTDB_NAME can
# arrive from OUTSIDE: the landing gate exports one shared fixture to everything it runs,
# and a suite under it that summons an aeon would hand that name straight to this function.
# Dropping there would delete the database the rest of the gate's suites are still using,
# mid-run, and every one of them would fail for a reason none of them could name.
fixture_drop() {
    [ -n "${FIXTURE_LIB:-}" ] && [ -n "${TESTDB_NAME:-}" ] || return 0
    # The library is read from the worktree, which an aeon may have deleted or renamed out
    # from under us by the time it exits; the installed copy answers the same question.
    [ -f "$FIXTURE_LIB" ] || FIXTURE_LIB="$SPIRA_HOME/testdb.sh"
    [ -f "$FIXTURE_LIB" ] || return 0
    ( TESTDB_SHARED=0; . "$FIXTURE_LIB" && testdb_drop ) >/dev/null 2>&1
    return 0
}
# gate_unfinished -> 0, printing why, if a gate for this branch is still deciding.
#
# ASKED THROUGH gate-run.sh rather than reimplemented here, because that is the only thing
# holding both witnesses: its own state directory when the gate was started through it, and
# the process table when it was not — an agent that ran `gate.sh` in its own foreground and
# had it moved to the background leaves no state at all. Binding only the well-behaved path
# would miss exactly the mistake this exists for (law-guard-binds-the-caller).
#
# Exit 2 is its "still deciding". EVERY OTHER ANSWER, including a missing runner, means
# nothing is in flight: this must never invent a reason to withhold an attempt, or a session
# that failed at its own work would stop counting toward poison.
gate_unfinished() {
    local out st
    [ -f "$SPIRA_HOME/gate-run.sh" ] || return 1
    out="$(bash "$SPIRA_HOME/gate-run.sh" --status "$BRANCH" "$REPO_NAME" 2>/dev/null)"; st=$?
    [ "$st" -eq 2 ] || return 1
    printf '%s' "$out"
    return 0
}

cleanup() {
    local rc=$? reset_at gate_why cause
    # `set -e` IS DISARMED FOR THE WHOLE OF TEARDOWN, first line, before anything can fail.
    # This ran under errexit and every step of it was one failing command away from being
    # skipped in silence — which is what happened: a compare-and-swap release exits non-zero
    # on a mismatch, so the shell died inside its own EXIT trap between the bump and the log
    # line. A whole window of aeons wrote no `done` ledger line and released no bead, and the
    # ledger's own measurement went with them. A teardown must run to the end regardless: it
    # is the last chance to record what happened. Note that `[ -n "$X" ] && cmd` is itself
    # one of those failing commands whenever $X is empty.
    set +e
    if [ -n "$HB_PID" ]; then
        # Kill the heartbeat's children (e.g., the current `sleep`) BEFORE signalling the
        # subshell. Without this, `kill "$HB_PID"` exits the subshell but leaves the
        # sleeping child alive in the caller's process group — which the suite runner
        # detects as a background-job leak and marks the suite red. (sp-6a72t)
        # Also wait after the kill: the heartbeat process remains in the suite's process
        # group until reaped, and an unwaited HB_PID triggers the same "left background
        # jobs" check even when the sleep child is already gone. (sp-1ux75)
        _hb_kids="$(ps --ppid "$HB_PID" -o pid= 2>/dev/null | tr -s ' \n' ' ')"
        [ -n "${_hb_kids// /}" ] && kill $_hb_kids 2>/dev/null || true
        kill "$HB_PID" 2>/dev/null; wait "$HB_PID" 2>/dev/null || true
        unset _hb_kids
    fi
    fixture_drop
    rm -f "$SPIRA_RUN/aeon/$BEAD_ID.lease"
    # RESTORE THE WORLD if this aeon stopped it. Runs here, after the heartbeat and fixture
    # but before any bead operations, so it fires on every exit path — a world halted for a
    # bead that fails must not stay halted because the aeon died mid-teardown.
    if [ "${WORLD_WAS_STOPPED:-0}" = 1 ]; then
        log "$FAYTH: $BEAD_ID $SPIRA_WORLD_STOP_LABEL bead — starting the world"
        "$SPIRA_HOME/world.sh" start >/dev/null 2>&1 || true
    fi
    cd "$REPO" 2>/dev/null || true
    # If the bead is still ours and still open, hand it back rather than holding a lease
    # nobody is working. Lease expiry would do this eventually; doing it now is honest.
    #
    # release_own_claim (lib.sh), never a hand-written unclaim. It compares against
    # BEADS_ACTOR, and the holder is this aeon's own name — `aeon-mindy`, not `aeon-builder`
    # — because the claim is made under BEADS_ACTOR, which took a per-instance name the day
    # aeons got identities. Release sites that derived the actor a second time as
    # "aeon-$FAYTH" compared against a string no bead has ever carried: every release
    # silently no-opped, `bd` exited non-zero into >/dev/null, and the bead sat in_progress
    # until its lease expired and strand.sh ghost-reclaimed it — CHARGING A SECOND ATTEMPT
    # for the release this line was supposed to perform. "Returned unchanged" is not
    # achievable without it: a bead still in_progress has not been returned at all. One
    # function and no copies, because deriving the name twice is what let the two disagree.
    st="$(bdjson show "$BEAD_ID" 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: print(""); sys.exit()
d=d if isinstance(d,list) else [d]
print(d[0].get("status","") if d else "")' 2>/dev/null)"
    if [ "$st" != "closed" ]; then
        # CHARGING IS DEFAULT-DENY. An attempt is charged ONLY when the harness can say
        # what the WORK did wrong — not "this bead has been touched three times" but "we
        # know this work keeps failing" (law-alerts-must-be-actionable). aeon_disposition
        # (lib.sh) holds the precedence between the branches below and decides; this block
        # only gathers the inputs it needs (short-circuited in the same precedence order, so
        # a marker a higher branch would also match is never consumed early) and performs
        # the side effects the verdict names.
        local _d_cap_rc=1 _d_reset="" _d_slain=no _d_thrash=no _d_thrash_note="" \
            _d_thrash_tip="" _d_thrash_charged=no _d_thrash_streak=0 _d_lapsed=no \
            _d_lapsed_quiet="" _d_lapsed_last="" _d_gate=no _d_gate_why="" _d_decision=no \
            _d_operator=no _d_yield=no _d_outcome="-" _d_line _d_status _d_charge \
            _d_reqcause _d_notekey _lapsed_body _lapsed_ts _rq_count

        if reset_at="$(capacity_reset_at "$LOGF")"; then
            _d_cap_rc=0; _d_reset="$reset_at"
        elif [ -f "$SPIRA_RUN/$BEAD_ID.slain" ]; then
            _d_slain=yes
        elif [ -f "$SPIRA_RUN/$BEAD_ID.thrash" ]; then
            # thrash_streak_bump compares this branch's tip to the tip recorded at the LAST
            # thrash on this bead (bead metadata, survives across summons unlike $SPIRA_RUN).
            # Same tip bumps the streak; a moved tip — including a first-ever thrash — resets
            # it to 1. At or over SPIRA_THRASH_STREAK_CAP consecutive same-tip thrashes the
            # requeue IS charged (sp-gs24i: five summons across seven hours, none charged).
            _d_thrash=yes
            _d_thrash_note="$(cat "$SPIRA_RUN/$BEAD_ID.thrash" 2>/dev/null)"
            rm -f "$SPIRA_RUN/$BEAD_ID.thrash"
            _d_thrash_tip="$(git -C "${WORK:-/dev/null}" rev-parse --short HEAD 2>/dev/null || echo ?)"
            _d_thrash_streak="$(thrash_streak_bump "$BEAD_ID" "$_d_thrash_tip" "${_d_thrash_note:-?}")"
            [ "${_d_thrash_streak:-0}" -ge "${SPIRA_THRASH_STREAK_CAP:-2}" ] 2>/dev/null && _d_thrash_charged=yes
        elif [ -f "$SPIRA_RUN/$BEAD_ID.lapsed" ]; then
            _d_lapsed=yes
            _lapsed_body="$(cat "$SPIRA_RUN/$BEAD_ID.lapsed" 2>/dev/null)"
            _d_lapsed_quiet="${_lapsed_body%%$'\t'*}"
            _d_lapsed_last="${_lapsed_body#*$'\t'}"
            rm -f "$SPIRA_RUN/$BEAD_ID.lapsed"
        elif gate_why="$(gate_unfinished)"; then
            _d_gate=yes; _d_gate_why="$gate_why"
        elif ! bdjson show "$BEAD_ID" 2>/dev/null | python3 -c '
import sys, json, os
ask = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
bead_id = sys.argv[1] if len(sys.argv) > 1 else ""
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d if isinstance(d, list) else [d]
if not d: sys.exit(0)
deps = d[0].get("dependencies") or []
close_sfx = " for bead " + bead_id if bead_id else None
open_ask = [x for x in deps
            if x.get("status") != "closed"
            and ask in (x.get("labels") or [])
            and (x.get("dependency_type") or x.get("type")) == "blocks"
            and not (close_sfx
                     and "Close GitHub issue " in (x.get("title") or "")
                     and close_sfx in (x.get("title") or ""))]
if open_ask: sys.exit(1)
sys.exit(0)' "$BEAD_ID" 2>/dev/null; then
            _d_decision=yes
        elif [ "${SESSION_RC:-0}" = 124 ] && [ "${committed:-}" != yes ]; then
            : # timeout — aeon_disposition reads SESSION_RC/committed directly, nothing to gather
        elif [ -n "$REQUEUE_CAUSE" ]; then
            : # harness requeue — REQUEUE_CAUSE/REQUEUE_WHY are already set by the verdict block
        elif [ -f "$SPIRA_RUN/$BEAD_ID.operator-wait" ]; then
            _d_operator=yes
        elif session_yield_headless "$LOGF"; then
            _d_yield=yes
        elif [ "${SESSION_STARTED:-0}" = 0 ]; then
            : # pre-session death — nothing to gather
        else
            _d_outcome="$(session_outcome "$LOGF")"
        fi

        _d_line="$(aeon_disposition "${st:-?}" "$_d_cap_rc" "$_d_slain" "$_d_thrash" \
            "$_d_thrash_charged" "$_d_lapsed" "$_d_gate" "$_d_decision" "${SESSION_RC:-0}" \
            "${committed:-no}" "${REQUEUE_CAUSE:-"-"}" "$_d_operator" "$_d_yield" \
            "${SESSION_STARTED:-0}" "$_d_outcome")"
        read -r _d_status _d_charge _d_reqcause _d_notekey <<<"$_d_line"
        [ "$_d_reqcause" = "-" ] && _d_reqcause=""

        case "$_d_notekey" in
        capacity)
            # A spent capacity window must also shut the summoner, which a bump/note alone
            # cannot: this is the one branch besides the marker cleanups with a side effect
            # of its own.
            capacity_pause_set "$_d_reset" "$BEAD_ID"
            release_own_claim "$BEAD_ID"
            bdq note "$BEAD_ID" "Returned unchanged by aeon.sh: the account's capacity window was spent mid-session, so this bead was never judged. No attempt was charged and nothing about the work is implied. Summoning is paused until the window reopens." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID returned unchanged — the account ran out of capacity, no attempt charged"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        slain)
            # SLAIN IS NOT FAILED: an operator stopping an aeon says nothing about the work.
            release_own_claim "$BEAD_ID"
            log "$FAYTH: $BEAD_ID slain — released, no attempt charged"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        thrash-charged)
            release_own_claim "$BEAD_ID"
            bump_requeue "$BEAD_ID" "$_d_reqcause"
            bdq note "$BEAD_ID" "STICKING POINT: ${_d_thrash_note:-?}

Requeued (thrash): the deliverable did not move for ${SPIRA_THRASH_MINUTES:-20}m while turns advanced, and this is the ${_d_thrash_streak}th consecutive thrash with branch $BRANCH still at $_d_thrash_tip — nothing has been committed since the last one. An attempt IS charged this time: the sticking point above is the next aeon's first move, not something to rediscover by reading back through this bead's notes." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID thrash-requeued — attempt charged (streak $_d_thrash_streak, tip $_d_thrash_tip unchanged; last: ${_d_thrash_note:-?})"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        thrash)
            release_own_claim "$BEAD_ID"
            bump_requeue "$BEAD_ID" "$_d_reqcause"
            bdq note "$BEAD_ID" "Requeued (thrash): the deliverable did not move for ${SPIRA_THRASH_MINUTES:-20}m while turns advanced. Last action: ${_d_thrash_note:-?}. No attempt charged — the next aeon should start from this sticking point." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID thrash-requeued — no attempt charged (streak $_d_thrash_streak, tip $_d_thrash_tip; last: ${_d_thrash_note:-?})"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        lapsed)
            # LEASE LAPSE IS A VERDICT, unlike slay or thrash: the attempt IS charged so a
            # bead nothing can finish reaches the escalation threshold. Branch and worktree
            # are preserved (the kill left them intact) so attempt 2 can continue.
            bump_lapsed "$BEAD_ID" "${_d_lapsed_last:-?}"
            mkdir -p "$SPIRA_RUN/lapsed"
            _lapsed_ts="$(date -u +%Y%m%dT%H%M%SZ)"
            printf 'bead: %s\nquiet: %ss\nlast: %s\nbranch: spira/%s\ntip: %s\n' \
                "$BEAD_ID" "${_d_lapsed_quiet:-?}" "${_d_lapsed_last:-?}" \
                "$BEAD_ID" "$(git -C "${WORK:-/dev/null}" rev-parse --short HEAD 2>/dev/null || echo ?)" \
                > "$SPIRA_RUN/lapsed/$BEAD_ID-$_lapsed_ts"
            bdq note "$BEAD_ID" "Lease lapsed: the trace was silent for ${_d_lapsed_quiet:-?}s (limit ${FAYTH_LEASE_SECONDS:-600}s). Last: ${_d_lapsed_last:-?}. Branch spira/$BEAD_ID preserved. Attempt 2 should start from where attempt 1 wedged." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID lease lapsed — attempt charged (quiet ${_d_lapsed_quiet:-?}s)"
            release_own_claim "$BEAD_ID"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        gate-unfinished)
            release_own_claim "$BEAD_ID"
            bdq note "$BEAD_ID" "Released by aeon.sh: the session ended while its landing gate was still running, so it never held a verdict about its own work. No attempt was charged and nothing about the work is implied — $_d_gate_why. Run the gate through gate-run.sh, which waits in bounded slices, and do not end the session while it is unfinished." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID released with its gate still running — no attempt charged ($_d_gate_why)"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        decision-blocked)
            release_own_claim "$BEAD_ID"
            bump_requeue "$BEAD_ID" "$_d_reqcause"
            bdq note "$BEAD_ID" "Released by aeon.sh: blocked on an open decision bead (${SPIRA_ASK_LABEL:-needs-operator} label) — waiting for operator input. No attempt charged; the bead becomes ready when the decision is resolved." >/dev/null 2>&1  # literal-ok: human-readable note, not a label predicate
            log "$FAYTH: $BEAD_ID has open decision blocker — released, no attempt charged"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        timeout)
            # rc=124 is `timeout`'s own exit code for a process it killed. Combined with
            # nothing committed, this is the harness's clock ending the turn, not a verdict
            # about the work (sp-l7f5, the same reading as a slain aeon).
            bdq note "$BEAD_ID" "Timeout: the session was killed by the lane cap (${FAYTH_TIMEOUT_SECONDS:-?}s) with nothing committed. This is the harness's clock ending the turn, not a verdict about the work. No attempt charged." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID timed out — no attempt charged"
            release_own_claim "$BEAD_ID"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        requeue)
            # THE BEAD IS OPEN BECAUSE THIS SCRIPT REOPENED IT. session_outcome cannot see
            # that — a session that committed, closed the bead and ran to its own end reads
            # `unlanded`, the one outcome that charges — so this is checked ahead of it.
            bump_requeue "$BEAD_ID" "$_d_reqcause"
            _rq_count="$(requeues_of "$BEAD_ID")"
            bdq note "$BEAD_ID" "Requeue $_rq_count ($_d_reqcause): $REQUEUE_WHY The session did the work and closed the bead; the harness put it back. NO attempt was charged and nothing about the work is implied." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID requeued by the harness ($_d_reqcause, count $_rq_count) — no attempt charged"
            release_own_claim "$BEAD_ID"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        operator-wait)
            rm -f "$SPIRA_RUN/$BEAD_ID.operator-wait"
            bump_requeue "$BEAD_ID" "$_d_reqcause"
            bdq note "$BEAD_ID" "Released by aeon.sh: the session sent a kind-question mail to the operator and exited awaiting a reply. No attempt charged; the bead becomes ready when the question is answered." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID operator-wait — sent kind-question mail, released, no attempt charged"
            release_own_claim "$BEAD_ID"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        yield-headless)
            # rc=0 with the bead open reads "finished, didn't close" with no signal that
            # background tasks were killed mid-flight; named here rather than left generic
            # `unlanded` so the next summon gets a specific repair.
            bdq note "$BEAD_ID" "Yield-headless: the session ended its turn waiting for a background task notification. This session runs headless — there is no notification channel, so the session terminated and its background tasks were killed. Attempt charged; commit before any long step rather than backgrounding and yielding." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID yield-headless — ended turn waiting for background task, attempt charged"
            release_own_claim "$BEAD_ID"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        pre-session)
            # rc=0 with wall_s=? is structurally impossible: if the session never ran, the
            # exit was a failure. Override any accidental zero so the ledger invariant holds.
            [ "$rc" -eq 0 ] && rc=1
            bdq note "$BEAD_ID" "Pre-session death (rc=$rc): the aeon died during setup before its Claude session started. Attempt charged — this failure repeats until the box state changes." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID pre-session death (rc=$rc) — attempt charged"
            release_own_claim "$BEAD_ID"
            ledger_done "$rc" "$_d_status"
            exit $rc ;;
        unlanded)
            bdq note "$BEAD_ID" "Unlanded ($_d_outcome): the session ran to its own end and left this bead open. That is a verdict about the work; the next claim counts toward the poison threshold via the events trail." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID not closed ($_d_outcome), released"
            release_own_claim "$BEAD_ID" ;;
        not-judged)
            bump_requeue "$BEAD_ID" "$_d_reqcause"
            bdq note "$BEAD_ID" "Not judged ($_d_outcome): the worker did not survive to judge this bead, so NO attempt was charged and nothing about the work is implied. See $LOGF." >/dev/null 2>&1
            log "$FAYTH: $BEAD_ID never judged ($_d_outcome) — no attempt charged"
            release_own_claim "$BEAD_ID" ;;
        esac
    elif [ -f "$SPIRA_HOME/gate-run.sh" ]; then
        local gate_st _cert_mode _cert_ahead_subjects _cert_hasown _cert_tip _cert_ls_state _cert_ls_tip _cert_out _cert_rc
        gate_why="$(bash "$SPIRA_HOME/gate-run.sh" --status "$BRANCH" "$REPO_NAME" 2>/dev/null)"; gate_st=$?
        # A CLOSED, COMMITTED BRANCH MUST NOT DEPEND ON THE SESSION HAVING CALLED
        # queue.sh submit ITSELF (sp-u9f82). A branch ahead of the base is certified here,
        # by the harness, unless it is CERTIFIED already.
        #
        # QUEUE MODE ONLY. Every other mode's periodic landing pass already walks every
        # branch under refs/heads/spira/* and lands or reopens it without needing a
        # landstate record first — only queue mode's batch builder selects on CERTIFIED
        # alone, which is the one case a missing record actually strands a branch. In push
        # mode, "certify" IS "land": queue.sh submit rebases and pushes straight to the
        # base, a repository-wide side effect this file has no business causing mid-teardown.
        _cert_mode="$(repo_land "$REPO_NAME" 2>/dev/null)"
        # "AHEAD" MEANS A COMMIT NAMING THIS BEAD, not merely a nonzero count. sp-vd-deep
        # (test-aeon-verdict.sh) landed its own commit straight onto the base and was closed
        # over a branch that still carried unrelated leftover commits from an earlier,
        # abandoned attempt; those commits are ahead of the base but are nobody's fault this
        # bead answers for, and gating or certifying them in its name is simply wrong.
        if [ "$_cert_mode" = queue ]; then
            _cert_ahead_subjects="$(git -C "$REPO" log --format='%s%n%b' "$BASE_FQREF..$BRANCH" 2>/dev/null)"
            if grep -qF "$BEAD_ID" <<< "$_cert_ahead_subjects"; then _cert_hasown=1; else _cert_hasown=0; fi
        else
            _cert_hasown=0
        fi
        case "$gate_st" in
            2)  # CLOSED WITH THE GATE STILL RUNNING is not reopened: the work is committed,
                # and the landing pass gates the branch again before it merges. Must not be silent.
                bdq note "$BEAD_ID" "Closed by the session while its landing gate was still running — $gate_why. The close carries no gate verdict; the landing pass gates this branch again and reopens the bead if it fails." >/dev/null 2>&1
                log "$FAYTH: $BEAD_ID closed with its gate still running ($gate_why)" ;;
            1)  if [ "$_cert_mode" != queue ]; then
                    bdq note "$BEAD_ID" "Closed against a recorded FAIL verdict for this exact tree — ${gate_why:-gate returned fail}. The landing pass will reopen this bead." >/dev/null 2>&1
                    log "$FAYTH: $BEAD_ID closed against a recorded FAIL gate verdict"
                elif [ "$_cert_hasown" != 1 ]; then
                    log "$FAYTH: $BEAD_ID closed against a recorded FAIL gate verdict, but $BRANCH carries no commit of $BEAD_ID's own ahead of base — not this bead's fault, not reopening"
                else
                    # A FAIL IS ALREADY ON RECORD FOR THIS EXACT TREE — gate-run.sh's key
                    # covers both the branch tip and the base tip, so this verdict is about
                    # the tree as it stands, not a stale one. Reopen on it now rather than
                    # defer to the landing pass, which would only rediscover the same FAIL at
                    # the cost of a full gate re-run: gate.sh's verdict cache holds a PASS,
                    # never a FAIL.
                    bead_reopen "$BEAD_ID" cert-gate-red "Reopened by aeon.sh: closed against a recorded FAIL gate verdict for $BRANCH in $REPO_NAME.

$gate_why"
                    st=open
                    log "$FAYTH: $BEAD_ID REOPENED — closed against a recorded FAIL gate verdict"
                fi ;;
            3)  if [ "$_cert_mode" != queue ]; then
                    bdq note "$BEAD_ID" "Closed without ever obtaining a gate verdict — no gate ran or finished for this branch." >/dev/null 2>&1
                    log "$FAYTH: $BEAD_ID closed with no gate verdict (none ran)"
                elif [ "$_cert_hasown" != 1 ]; then
                    bdq note "$BEAD_ID" "Closed without ever obtaining a gate verdict — no gate ran or finished for this branch. $BRANCH carries no commit of $BEAD_ID's own ahead of $REPO_NAME's base, so there is nothing to certify." >/dev/null 2>&1
                    log "$FAYTH: $BEAD_ID closed with no gate verdict (none ran) — $BRANCH has no commit naming $BEAD_ID ahead of base, nothing to certify"
                else
                    _cert_tip="$(git -C "$REPO" rev-parse "$BRANCH" 2>/dev/null)"
                    _cert_ls_state=""; _cert_ls_tip=""
                    read -r _cert_ls_state _cert_ls_tip _ <<< "$(land_state "$BEAD_ID" 2>/dev/null)"
                    if [ "${_cert_ls_state:-}" = CERTIFIED ] && [ -n "${_cert_tip:-}" ] && [ "${_cert_ls_tip:-}" = "$_cert_tip" ]; then
                        log "$FAYTH: $BEAD_ID closed with no gate verdict, but $_cert_tip is already CERTIFIED — the session submitted it itself"
                    else
                        _cert_out="$(bash "$SPIRA_HOME/queue.sh" submit "$BRANCH" "$REPO_NAME" 2>&1)"; _cert_rc=$?
                        if [ "$_cert_rc" -eq 0 ]; then
                            bdq note "$BEAD_ID" "Certified by aeon.sh: closed without ever obtaining a gate verdict, so aeon.sh certified $BRANCH itself rather than leave it stranded — $_cert_out" >/dev/null 2>&1
                            log "$FAYTH: $BEAD_ID certified by aeon.sh — $_cert_out"
                        else
                            bead_reopen "$BEAD_ID" cert-gate-red "Reopened by aeon.sh: closed without ever obtaining a gate verdict, so aeon.sh certified $BRANCH itself (queue.sh submit) and it failed.

$_cert_out"
                            st=open
                            log "$FAYTH: $BEAD_ID REOPENED — self-certification failed ($_cert_out)"
                        fi
                    fi
                fi ;;
        esac
    fi
    # PIDFILE IS REMOVED HERE, after all bead operations, so holder_alive stays true for the
    # entire teardown. strand-classify.py requires BOTH witnesses absent before classifying a
    # bead ghost: removing the pidfile early caused a bead whose aeon was mid-teardown to be
    # ghost-reclaimed when its lease expired during a long fixture_drop. (sp-nc74)
    rm -f "$PIDFILE" "${PIDFILE%.pid}.name"
    rm -rf "${SPIRA_MAIL:-}/aeon-${BEAD_ID:-}" 2>/dev/null || true
    ledger_done "${SESSION_RC:-$rc}" "${st:-?}"
    # A CLOSED BEAD IS A SUCCEEDED TASK. SESSION_RC is the claude CLI's exit code, held
    # separately because `rc=$?` at trap time reflects the verdict block's LAST COMMAND —
    # which may be a `bdq note` or `git` that returned non-zero for cosmetic reasons —
    # not the session's verdict. A named unit (spira-ops, spira-qa) left in FAILED state
    # because of a stray command exit code shows up in every `systemctl --state=failed`
    # check and drowns genuine failures (law-alerts-must-be-actionable).
    # Exit 0 when the bead is closed: the work succeeded.
    # Exit SESSION_RC otherwise: a session that ran and did not close the bead is a
    # genuine failure, and SESSION_RC carries the claude CLI's actual exit code.
    [ "${st:-}" = "closed" ] && exit 0
    exit "${SESSION_RC:-$rc}"
}
# WHY THE HARNESS ITSELF PUT THIS BEAD BACK, if it did. Set by the verdict block at the foot
# of this script, read by the teardown above. Empty is the state every session starts in and
# the only state a session that closes cleanly ever reaches.
#
# A VARIABLE AND NOT A RE-READ OF THE BEAD, because from outside there is nothing to read: a
# bead the harness reopened a second ago and a bead the session never closed are the same row
# — open, unclaimed, no verdict. Only the process that performed the reopen knows, and it
# knows for the few seconds between doing it and exiting.
REQUEUE_CAUSE=""; REQUEUE_WHY=""
# THE SESSION'S OWN EXIT CODE, held separately so cleanup can read it. `cleanup() { local
# rc=$?` captures the script's exit code at the time the trap fires, which is the last
# command before the fall-off — not the session's. SESSION_RC is set right after `rc=$?`
# captures the session and is the only witness to rc=124 in the teardown.
SESSION_STARTED=0
SESSION_RC=0
trap cleanup EXIT INT TERM

# Create the aeon's per-claim mailbox for mid-run messages, and export the vars the
# PostToolUse hook needs to address it.
if [ -n "${SPIRA_MAIL:-}" ]; then
    mkdir -p "$SPIRA_MAIL/aeon-$BEAD_ID/new" \
             "$SPIRA_MAIL/aeon-$BEAD_ID/cur" \
             "$SPIRA_MAIL/aeon-$BEAD_ID/tmp"
fi
export BEAD_ID SPIRA_MAIL="${SPIRA_MAIL:-}" SPIRA_MAIL_FROM="${FAYTH^} <${FAYTH}@spira>"

# ---- heartbeat: LIVENESS LEASE -------------------------------------------------------
# A fixed 10-minute lease. The trace file growing — even by one byte — renews it in full.
# If the lease lapses the aeon is killed, work is preserved, and the bead carries a nudge
# note for the next attempt. The fuse on the ops pane shows time left in the lease.
#
# WHY TRACE GROWTH IS SUFFICIENT. The CLI emits a tool_progress heartbeat every ~30s while
# blocked on a single tool call. Measured across 789 aeon traces: only Bash (max 600s),
# TaskOutput (max 600s), and Agent (max 210s) emit tool_progress — every other tool is
# silent. So any ongoing tool invocation keeps the trace growing and holds the lease, and
# Bash's 600s ceiling means no single call can outlast the 600s lease. Silence is the one
# state a wedged aeon reaches, and the one state that lets the lease lapse.
#
# THE CASE THIS REPLACES (sp-9ix, 2026-09-12). An aeon whose trailing trace line is a
# result — a turn boundary — and which then hangs. The old model_idle detector read
# elapsed_time_seconds on the trailing heartbeat: a result line reads as elapsed=0,
# classified "acting", idle reset to zero. So a session that completed a turn and hung
# reported "acting" forever and the stall counter never fired. 122 turns, 76 minutes, one
# unchanging ops pane line — caught only by eye. The lease has no such blind spot: silence
# is silence regardless of what the trailing JSON says.
(
    _hb_s=""
    trap 'kill "$_hb_s" 2>/dev/null; exit 0' TERM INT
    _lease_dur="${FAYTH_LEASE_SECONDS:-600}"
    _lease_dir="$SPIRA_RUN/aeon"
    _lease_file="$_lease_dir/$BEAD_ID.lease"
    _prev_mtime="$(stat -c %Y "$LOGF" 2>/dev/null || echo 0)"
    _now="$(date +%s)"
    _session_start="$_now"
    _deadline=$((_now + _lease_dur))
    mkdir -p "$_lease_dir"
    # WRITE BESIDE AND RENAME so readers never see a partial file.
    printf '%s' "$_deadline" > "${_lease_file}.tmp" && mv "${_lease_file}.tmp" "$_lease_file"
    while true; do
        sleep "${FAYTH_HEARTBEAT_SECONDS:-30}" & _hb_s=$!
        wait "$_hb_s" 2>/dev/null || break
        _cur_mtime="$(stat -c %Y "$LOGF" 2>/dev/null || echo 0)"
        _now="$(date +%s)"
        _dwall="${SPIRA_THRASH_MINUTES:-20}"
        _dfuse="$(aeon_fuse_minutes "$BEAD_ID" "$SPIRA_RUN/worktree/$BEAD_ID" "$REPO_NAME" 2>/dev/null)"
        case "$(hb_tick "$_prev_mtime" "$_cur_mtime" "$_now" "${_deadline:-0}" "${_dfuse:-?}" "$_dwall" "$_session_start")" in
        renew)
            # Trace grew — renew the lease in full.
            _prev_mtime="$_cur_mtime"
            _deadline=$((_now + _lease_dur))
            printf '%s' "$_deadline" > "${_lease_file}.tmp" && mv "${_lease_file}.tmp" "$_lease_file"
            ;;
        lapse)
            # Lease lapsed. Write the marker first so cleanup() takes the lapse path,
            # then kill the process group ($$=parent PID is also the PGID, so -$$ reaches
            # the claude session, aeon.sh, and this subshell together).
            _trailing="$(trace_last "$LOGF" 2>/dev/null | head -c 200)"
            _quiet=$((_now - _cur_mtime))
            printf '%s\t%s\n' "$_quiet" "${_trailing:-?}" > "$SPIRA_RUN/$BEAD_ID.lapsed"
            log "$FAYTH: $BEAD_ID lease lapsed (trace quiet ${_quiet}s, last: ${_trailing:-?}) — killing"
            kill -TERM -$$ 2>/dev/null
            exit 0
            ;;
        thrash)
            # DELIVERABLE-PROGRESS WALL. Catches an aeon whose turns advance (trace grows,
            # lease renews) while commits and worktree writes do not — both the fuse AND
            # the session's own elapsed time must exceed the wall (sp-sv34w): a bead
            # reclaimed after sitting idle has a stale fuse, and the session cannot have
            # stalled for longer than it has been alive. Kill the process group (-$$), same
            # as the lease-lapse path: a kill to $$ alone defers, since bash defers TERM
            # while waiting for a foreground process, so the claude session runs on past
            # the declared stall until it exits on its own.
            _dsess=$(( (_now - _session_start) / 60 ))
            _dlast="$(trace_last "$LOGF" 2>/dev/null | head -c 300)"
            printf '%s\n' "${_dlast:-no last action}" > "$SPIRA_RUN/$BEAD_ID.thrash"
            log "$FAYTH: $BEAD_ID deliverable stalled ${_dfuse}m session ${_dsess}m (wall ${_dwall}m) — requeueing for thrash"
            kill -TERM -$$ 2>/dev/null
            exit 0
            ;;
        esac
        bdq heartbeat "$BEAD_ID" >/dev/null 2>&1 || exit 0
    done
) & HB_PID=$!

# ---- workspace -----------------------------------------------------------------------
# A WORKTREE, never a checkout in $REPO. The first supervised run checked its branch out
# in the shared tree and moved the interactive session's HEAD out from under it — two
# agents on one working copy, where the second one's next commit lands on the first one's
# branch. A worktree gives the aeon its own directory against the same object store.
WORK="$SPIRA_RUN/worktree/$BEAD_ID"

# THE BASE IS A FRESHLY FETCHED REMOTE-TRACKING REF, NEVER THE LOCAL BRANCH. Nothing in this
# harness advances the shared checkout's default branch — the sentinel lands by pushing from
# the .landing worktree and never pulls the home checkout — so the local ref is however stale
# the last human left it. Every Spira bead edits the same few files under .claude/spira and
# the queue is serialised at one aeon, so a branch cut from that stale ref collides with
# whatever landed while the previous aeon worked, by construction, every time: sp-poison-retry
# was based at 8ed6cca with main already at 5da8438 and conflicted in sentinel.sh, lib.sh,
# gate.sh and log.md, none of which it had touched.
#
# AND IT IS NOT NECESSARILY NAMED `main`. Some repositories default to `master` and
# have no ref named `main` anywhere; the old answer named a branch that does not exist, so
# `git worktree add -b spira/<id> "$WORK" "$BASE"` below failed and NO AEON COULD GET A
# WORKSPACE in either of them. A base that cannot be resolved is fatal rather than guessed —
# working a bead against a branch nobody chose is worse than not working it.
#
# Resolved before the fetch and the fetch aimed at the base's own remote: `git fetch origin`
# was literal here, and a remote need not be called `origin`. The failure is not silent by
# design — a stale base is exactly the defect above, so say so in the log.
BASE="$(spira_landref "$REPO")" || {
    log "$FAYTH: $BEAD_ID names repo:$REPO_NAME, whose land ref cannot be resolved"
    bdq note "$BEAD_ID" "Released by aeon.sh: repo:$REPO_NAME has no resolvable default branch — $SPIRA_REPO_MAP declares no \`base\` for it, its remote publishes no HEAD, and it is not a local-only repository. Give it a base column. Refusing to guess: a branch cut from a guessed base rebases onto a ref nobody chose, and \`main\` is a guess that is wrong wherever a repository still uses \`master\`." >/dev/null 2>&1
    exit 1; }   # the EXIT trap unclaims it and writes the ledger line
BASE_BRANCH="$(ref_branch "$BASE")"
BASE_REMOTE="$(ref_remote "$BASE")" || BASE_REMOTE=""
if [ -n "$BASE_REMOTE" ]; then
    git -C "$REPO" fetch -q "$BASE_REMOTE" 2>/dev/null \
        || log "$FAYTH: fetch of $BASE_REMOTE failed — basing on a possibly stale $BASE"
fi
BASE_FQREF="$(qualify_base_ref "$BASE" "$REPO")"

# A WORKTREE PATH IS KEYED ON THE BEAD, AND A BEAD'S REPOSITORY CAN CHANGE. `repo:` is a
# label, and repointing one is a deliberate mechanism: the landing gate refuses a branch cut
# in the wrong repository, and the answer is to correct the label so the next aeon works it
# in the right checkout. But $WORK is the same path either way, and reusing whatever is
# there made that correction unenforceable — a repointed bead kept the OLD repository's
# worktree, and every summon after it attached to that tree and was handed a checkout in
# which the files the bead names do not exist. The repoint had no effect and could have
# none, forever, and nothing said so: `git worktree add` was never reached, so no command
# failed.
#
# The stale tree is MOVED ASIDE, never removed (worktree_evict_foreign, lib.sh). It may hold
# uncommitted work from a dead aeon, and a harness that deletes a tree to unblock itself is
# one that can destroy the only copy of something.
aside="$(worktree_evict_foreign "$WORK" "$REPO")"; evicted=$?
if [ "$evicted" = 2 ]; then
    die "$WORK belongs to another repository and could not be moved aside"
elif [ "$evicted" = 0 ]; then
    log "$FAYTH: $WORK was a worktree of another repository — moved to $aside"
    bdq note "$BEAD_ID" "Moved aside by aeon.sh: the worktree at $WORK belonged to a different repository than this bead's repo:$REPO_NAME. It is preserved at $aside — nothing was deleted — and a fresh worktree was cut in the right checkout. A bead whose repo: label is corrected keeps its old worktree path, so without this every later summon would go on working it in the old repository." >/dev/null 2>&1
fi

if [ ! -d "$WORK/.git" ] && [ ! -f "$WORK/.git" ]; then
    mkdir -p "$(dirname "$WORK")"
    # Through the chokepoint. A bare prune drops the registration of any worktree whose
    # `.git` link is unreadable even when the directory is intact and full of work, which
    # frees that branch for deletion and leaves a live tree registered nowhere. Here that
    # tree could be another aeon's, since one prune covers the whole repository.
    spira_prune_worktrees "$REPO" >/dev/null 2>&1
    if git -C "$REPO" show-ref --verify -q "refs/heads/$BRANCH"; then
        # A retry: the branch survives from a previous attempt. Reuse it rather than
        # refusing, and bring it current below.
        #
        # A BRANCH CHECKED OUT SOMEWHERE ELSE IS NEVER ADOPTED (law-one-aeon-one-worktree).
        # git refuses to attach a second worktree to one branch, so this `add` can fail for
        # two different reasons and they get two different answers:
        #
        #   - the holder is shaped like ANOTHER BEAD'S OWN canonical worktree ($SPIRA_RUN/
        #     worktree/<other-id>) — every aeon computes that path the same way, so a live
        #     match there names a live sibling, not a leftover. sp-zs04v's three sessions in
        #     one directory came from adopting exactly this case; the fix is to refuse and
        #     say whose worktree it is, not to work inside it.
        #   - the holder is NOT shaped like any bead's canonical worktree — a hand-made tree,
        #     a slaying that left its directory, a previous naming convention for this same
        #     bead's own branch (measured 2026-09-07: worktree/sp-2tv.spira, made by hand
        #     while the stale brain tree occupied the real path, held spira/sp-2tv; every
        #     summon after the brain tree was cleared died here instead of moving it aside,
        #     and the bead reached attempt 20). This one IS this bead's own stale tree: move
        #     it aside — never delete, it may hold uncommitted work — and cut $WORK fresh.
        _wt_tmp="$(mktemp)"; _wt_err=""
        if ! git -C "$REPO" worktree add -q "$WORK" "$BRANCH" 2>"$_wt_tmp"; then
            _wt_err="$(cat "$_wt_tmp" 2>/dev/null)"
            _held="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
                     | awk -v b="refs/heads/$BRANCH" '''/^worktree /{w=$2} /^branch /{if ($2==b) print w}''' | head -1)"
            _held_id=""
            case "$_held" in
                "$SPIRA_RUN/worktree/"*) _held_id="${_held#"$SPIRA_RUN/worktree/"}" ;;
            esac
            if [ -z "$_held" ] || [ ! -d "$_held" ]; then
                die "could not attach a worktree at $WORK to existing branch $BRANCH${_wt_err:+: $_wt_err}"
            elif [ -n "$_held_id" ] && [ "$_held_id" != "$BEAD_ID" ]; then
                # THE RECORDED branch: LABEL NAMED SOMEONE ELSE'S LIVE WORKTREE. `bd create
                # --parent` copies every label from the parent onto a child, including
                # branch: — a bead can carry a recorded branch it never created and that is
                # live under another bead's id right now. Dying here treated that mislabel as
                # fatal to the attempt: 18 mislabeled children died within a minute of being
                # claimed and outranked every other ready bead — the resume preference tries
                # them FIRST — running the whole fleet at ~2 builders for about an hour. The
                # label was wrong, not the bead, so correct it and take a fresh branch of this
                # bead's own rather than refusing the attempt.
                log "$FAYTH: $BEAD_ID: recorded branch $BRANCH is checked out at $_held, which belongs to $_held_id, not $BEAD_ID — a mislabeled branch:, not a resume; taking a fresh branch instead of dying"
                bdq note "$BEAD_ID" "Corrected by aeon.sh: this bead's recorded branch: label named $BRANCH, which belongs to $_held_id, not $BEAD_ID. Reset to spira/$BEAD_ID and started fresh." >/dev/null 2>&1
                BRANCH="spira/$BEAD_ID"
                bdq set-state "$BEAD_ID" "branch=$BRANCH" >/dev/null 2>&1
                unset _held _held_id aside
                rm -f "$_wt_tmp"; _wt_tmp="$(mktemp)"; _wt_err=""
                if git -C "$REPO" show-ref --verify -q "refs/heads/$BRANCH"; then
                    if ! git -C "$REPO" worktree add -q "$WORK" "$BRANCH" 2>"$_wt_tmp"; then
                        _wt_err="$(cat "$_wt_tmp" 2>/dev/null)"
                        die "could not attach a worktree at $WORK to this bead's own branch $BRANCH after correcting a mislabeled branch${_wt_err:+: $_wt_err}"
                    fi
                else
                    if ! git -C "$REPO" worktree add -q -b "$BRANCH" "$WORK" "$BASE_FQREF" 2>"$_wt_tmp"; then
                        _wt_err="$(cat "$_wt_tmp" 2>/dev/null)"
                        die "could not create a worktree at $WORK from $BASE after correcting a mislabeled branch${_wt_err:+: $_wt_err}"
                    fi
                fi
            else
                aside="$(worktree_move_aside "$_held" prior)"
                if [ -z "$aside" ]; then
                    die "$BRANCH is checked out at $_held and could not be moved aside"
                fi
                # THE MOVE RELOCATES THE DIRECTORY; IT DOES NOT FREE THE BRANCH. Git allows
                # exactly one worktree attached to a branch regardless of path, so the retry
                # below would fail again — now against $aside instead of $_held — unless the
                # moved tree's HEAD is detached first. Detaching changes only what ref HEAD
                # names: same commit, same index, same working tree, so nothing uncommitted
                # in $aside is touched.
                git -C "$aside" checkout -q --detach >/dev/null 2>&1
                log "$FAYTH: $BEAD_ID — $BRANCH was checked out at $_held (a previous path); moved aside to $aside, cutting $WORK fresh"
                bdq note "$BEAD_ID" "Moved aside by aeon.sh: a stale worktree at $_held held this bead's own branch $BRANCH under a previous path. Preserved at $aside — nothing deleted — and a fresh worktree cut at $WORK." >/dev/null 2>&1
                rm -f "$_wt_tmp"; _wt_tmp="$(mktemp)"
                if ! git -C "$REPO" worktree add -q "$WORK" "$BRANCH" 2>"$_wt_tmp"; then
                    _wt_err="$(cat "$_wt_tmp" 2>/dev/null)"
                    die "could not attach a worktree at $WORK to existing branch $BRANCH after moving aside $_held${_wt_err:+: $_wt_err}"
                fi
            fi
            unset _held _held_id aside
        fi
        rm -f "$_wt_tmp"; unset _wt_tmp _wt_err
    else
        _wt_tmp="$(mktemp)"; _wt_err=""
        if ! git -C "$REPO" worktree add -q -b "$BRANCH" "$WORK" "$BASE_FQREF" 2>"$_wt_tmp"; then
            _wt_err="$(cat "$_wt_tmp" 2>/dev/null)"
            die "could not create a worktree at $WORK from $BASE${_wt_err:+: $_wt_err}"
        fi
        rm -f "$_wt_tmp"; unset _wt_tmp _wt_err
    fi
fi

# A RETRY inherits whatever base its first attempt was cut from, so a branch created before
# this rule existed — or one that sat while other work landed — is still stale here. Rebase
# it now, while the bead is claimed and nothing else can be inside: the claim is atomic and
# this process is the only holder, so there is no live aeon to rewrite commits beneath.
#
# A conflict is NOT an escalation and NOT a reason to refuse the bead. It is handed to the
# aeon as work, with the colliding paths named, because resolving it is exactly the kind of
# judgement an aeon is for and the alternative is a branch that fails its landing three
# times and poisons a bead nobody needed to look at.
REBASE_BRIEF=""
if ! rebase_branch "$BRANCH" "$BASE_FQREF" "$REPO" "$REPO_NAME"; then
    log "$FAYTH: $BRANCH does not rebase onto $BASE — conflicts in ${REBASE_CONFLICTS:-unknown}"
    REBASE_BRIEF="## Rebase your branch first

\`$BRANCH\` is behind \`$BASE\` and does not rebase onto it cleanly. It will not land until
it does, so this is part of the bead, not a reason to stop:

    git -C $WORK rebase $BASE

conflicts in: ${REBASE_CONFLICTS:-unknown}

Resolve every conflict, \`git add\` each file, \`git rebase --continue\`, then do the work.
A merge conflict is not an escalation — do not close the bead and do not ask about it.
"
fi

# RESUME_BRIEF — when a prior session committed work on this branch, tell the model
# explicitly so it resumes from that state rather than restarting from scratch.
#
# WHY AFTER THE REBASE. The count `BASE..BRANCH` is correct only once the branch sits on
# top of the current base. Before the rebase, the count might include commits already on
# the base (if the branch were merged rather than rebased); after, it is exactly the set
# of new commits the next session will see. An aeon that is told "5 commits from a prior
# session" before the rebase would be counting commits that no longer exist at that tip.
#
# ZERO MEANS FRESH. A branch the harness just created from the base has no prior commits
# and gets no brief — "resume rather than restart" is noise when there is nothing to resume.
_n_prior="$(git -C "$REPO" rev-list --count "$BASE_FQREF..$BRANCH" 2>/dev/null || true)"
_prior_log=""
case "${_n_prior:-0}" in
    0|'?') ;;
    *) _prior_log="$(git -C "$REPO" log --format='  %h %s' -n 5 "$BRANCH" 2>/dev/null)" ;;
esac
RESUME_BRIEF="$(render_resume_brief "$BRANCH" "$WORK" "$_n_prior" "$_prior_log")"

# SLAIN_BRIEF — when the last commit is a wip salvage from a slain session, the next aeon
# needs to know so it reviews it before building on it rather than assuming it is finished.
SLAIN_BRIEF=""
if [ "${_n_prior:-0}" -gt 0 ] 2>/dev/null; then
    _last_subject="$(git -C "$REPO" log --format='%s' -1 "$BRANCH" 2>/dev/null)"
    _slay_when="" _wip_diffstat=""
    case "$_last_subject" in
        *": wip — salvaged at slay ("*)
            _slay_when="$(git -C "$REPO" log --format='%ci' -1 "$BRANCH" 2>/dev/null)"
            _wip_diffstat="$(git -C "$REPO" diff --stat "$BASE_FQREF" "$BRANCH" 2>/dev/null | tail -1)"
            ;;
    esac
    SLAIN_BRIEF="$(render_slain_brief "$_last_subject" "$_n_prior" "$BASE" "$_slay_when" "$_wip_diffstat" "$LOGF")"
    unset _last_subject _slay_when _wip_diffstat
fi
unset _n_prior _prior_log

# ---- the assigned worktree, exported for the commit guard ----------------------------
# SPIRA_WORK is the canonical path of this aeon's worktree. Exported HERE, after the
# worktree path is fully settled (WORK may be redirected above when a branch is already
# checked out elsewhere), so every subprocess — including the model session and any git
# hook it triggers — inherits the value. branch-guard.sh staged reads it to refuse commits
# that happen outside this path (law-worktrees-in-the-sanctioned-root, rung 4).
export SPIRA_WORK="$WORK"
export SPIRA_FAYTH="$FAYTH"
if [ "$FAYTH" = czar ]; then
    SPIRA_CZAR_CLASS="$(printf '%s' "$claimed" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d if isinstance(d, list) else [d]
if not d: sys.exit(0)
print(next((l[11:] for l in (d[0].get("labels") or []) if l.startswith("czar-class:")), ""))' 2>/dev/null)"
    export SPIRA_CZAR_CLASS
    export SPIRA_CZAR_TRIGGER_BEAD="$BEAD_ID"
fi

# ---- pre-session dirty-files guard -----------------------------------------------
# AN AEON THAT RUNS `git add -A` IN A SHARED CHECKOUT STAGES WHATEVER HAPPENS TO BE
# DIRTY — archivist drafts, operator edits, any uncommitted change from any other process.
# The guard below snapshots what is dirty NOW (after the harness has finished its own setup
# but before the aeon's session starts) and installs a per-worktree pre-commit hook that
# refuses to land any of those paths in a commit. Polite refusal: the hook names the
# offending files so the aeon can exclude them with `git add -- <specific-path>`.
#
# PER-WORKTREE HOOKS require extensions.worktreeConfig in the parent repo (so git reads
# each worktree's own config file) and `git config --worktree core.hooksPath` to write to
# that worktree-specific config. Enabling worktreeConfig is idempotent and additive:
# worktrees without a config.worktree file behave exactly as before.
_wt_gitdir="$(git -C "$WORK" rev-parse --path-format=absolute --git-dir 2>/dev/null || true)"
if [ -n "$_wt_gitdir" ]; then
    git -C "$REPO" config extensions.worktreeConfig true 2>/dev/null || true

    # Tracked modifications and new untracked files present before the session starts.
    # Sorted so grep -xF can scan the list without requiring comm(1)'s sorted inputs.
    _dirty_snapshot="$_wt_gitdir/spira-dirty-before"
    { git -C "$WORK" diff --name-only HEAD 2>/dev/null
      git -C "$WORK" ls-files --others --exclude-standard 2>/dev/null
    } | sort -u >"$_dirty_snapshot"

    # EVERY HOOK, NOT JUST THIS ONE. This used to copy pre-commit-guard.sh in as the
    # worktree's only hook and point core.hooksPath at it. Because a per-worktree
    # hooksPath OVERRIDES the repo-level one, that silently disarmed every OTHER hook in
    # the repository for every aeon worktree — including the reference-transaction guard
    # written after an Ops aeon deleted 24 peers' unlanded branches from inside one, and
    # including the canonical pre-commit's own fences. The guard was verified by a live
    # probe in the main checkout, which is the one place an aeon never works (sp-urifb,
    # law-guard-proved-where-the-offender-runs).
    #
    # worktree-hooks.sh composes the directory from the canonical set rather than from a
    # list here, so a hook added to spira/hooks tomorrow is armed in worktrees without
    # anyone remembering this line exists. It is a separate script so the property can be
    # asserted without summoning an aeon; test-ref-guard.sh calls it directly.
    SPIRA_HOME="$SPIRA_HOME" bash "$SPIRA_HOME/worktree-hooks.sh" install "$WORK" >/dev/null 2>&1 || true
fi

DIRTY_BRIEF=""
if [ -n "${_wt_gitdir:-}" ] && [ -s "${_wt_gitdir}/spira-dirty-before" ]; then
    _dirty_list="$(sed 's/^/  /' "$_wt_gitdir/spira-dirty-before")"
    DIRTY_BRIEF="## Pre-existing dirty files — do not stage these

**These files were already modified or untracked in your worktree when this session started.** They belong to another process (an archivist, an operator, a prior session's scratch work) and must not appear in your commit.

\`\`\`
$_dirty_list
\`\`\`

**Never use \`git add -A\` or \`git add .\`** — they sweep everything and will pick these up. Stage only the paths you yourself wrote:

    git add -- <specific-path>

The pre-commit hook will refuse any commit that stages a pre-session path and name the offenders. Override (only when genuinely necessary):

    SPIRA_ALLOW_DIRTY_STAGE=1 git commit ..."
fi
unset _wt_gitdir _dirty_snapshot _dirty_hook_dir _dirty_list

# ---- one test fixture for the whole session -------------------------------------------
# WAITING ON TESTS WAS 43% OF A SESSION'S WALL CLOCK and 73% of its tool time, and the
# suites here are not CPU-bound, they are database-bound: `bd init` is nearly all of the
# ~27s (55s under load) each suite spends building its fixture. The landing gate already
# builds ONE and lets every suite reset it instead — 0.2s — but an aeon running suites by
# hand got none, so each of its ~15 single-suite runs paid the full build.
#
# So the aeon builds one too, here, once, and exports it into the session. Every Bash call
# the session makes inherits it, which is the whole mechanism: a suite that sources the
# fixture library sees TESTDB_SHARED and resets rather than rebuilds, with no argument to
# pass and nothing for the session to remember (law-gate-once-fixture-shared).
#
# BUILT FROM THE WORKTREE'S OWN COPY, not the installed one. The tree whose suites will
# consume the fixture is the tree that should build it — the same rule the landing gate
# follows — so a bead that changes what a baseline means changes both halves together.
# A repository with no such file gets no fixture and no error; that is what decides which
# repositories this applies to, rather than a list of names.
#
# AND ALWAYS ITS OWN. Whatever the caller had is cleared before anything else, and only what
# this aeon builds is put back — the landing gate exports one shared fixture to everything it
# runs, so a suite under it that summons an aeon hands that database straight through. Passed
# on, the session would reset a database it does not own, mid-run, while the caller's own
# suites were still reading it, and every one of them would fail for a reason none could name.
#
# THE COST IS ONE BUILD PER SESSION, PAID EVEN BY A BEAD THAT RUNS NO TESTS, and the log
# line below is the meter that says when that stops being a good trade (the number to watch
# is this build against the count of suite runs in the session's own trace).
unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_BIN TESTDB_SERVER_INIT_HASH
fixture_ms=0
if [ -f "$WORK/$SPIRA_TESTDB_LIB" ]; then
    fixture_err="$(mktemp)"
    fixture_t0="$(date +%s%3N)"
    # A subshell, so the fixture library's functions never enter the supervisor: this is
    # branch code, and aeon.sh is the process that decides whether the branch's bead may be
    # reclaimed. The four values it prints are the whole interface.
    #
    # THE REDIRECTION IS INSIDE THE SUBSTITUTION, and it has to be. A simple command that is
    # nothing but an assignment — `v="$(...)" 2>f` — performs its redirection in an
    # environment the substitution never sees, so the diagnosis went to this process's own
    # stderr and the file stayed empty. The log line below then promised a reason and carried
    # a colon and nothing else, which is worse than saying nothing: a reader takes an empty
    # reason for a failure that had none.
    fixture_out="$( {
        . "$WORK/$SPIRA_TESTDB_LIB" && testdb_up "aeon${BEAD_ID//[^a-zA-Z0-9]/}" >&2 &&
        printf '%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \
            "$TESTDB_NAME" "$TESTDB_DIR" "$TESTDB_BASELINE" "${TESTDB_BIN:-}" \
            "${TESTDB_STARTED_SERVICE:-0}" "${TESTDB_MODE:-embedded}" \
            "${TESTDB_SERVER_INIT_HASH:-}"
    } 2>"$fixture_err" )"
    fixture_ms=$(( $(date +%s%3N) - fixture_t0 ))
    if [ -n "$fixture_out" ]; then
        { read -r TESTDB_NAME; read -r TESTDB_DIR; read -r TESTDB_BASELINE; read -r TESTDB_BIN
          read -r TESTDB_STARTED_SERVICE; read -r TESTDB_MODE
          read -r TESTDB_SERVER_INIT_HASH; } <<< "$fixture_out"
        export TESTDB_SHARED=1 TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_BIN \
               TESTDB_STARTED_SERVICE TESTDB_MODE TESTDB_SERVER_INIT_HASH
        # PATH IS DELIBERATELY NOT TOUCHED, and this is the fix for the v53/v61 scar.
        # Prepending TESTDB_BIN here put a tempdir symlink `bd -> bd-embedded` (a tagged
        # release that knows 53 migrations) first on the PATH of the aeon's OWN shell, for
        # the aeon's whole life. Bare `bd` against the production store then printed
        # "schema version mismatch: database is at v61, binary knows up to v53" — and
        # printed it while EXITING 0. Five aeons read that as a broken database and each
        # escalated a destructive rollback of a store that was healthy.
        #
        # THE LINE WAS ALSO REDUNDANT. testdb_up() re-prepends TESTDB_BIN to PATH and
        # SPIRA_PATH itself whenever a suite enters a shared fixture (testdb.sh, the
        # TESTDB_SHARED branch), so every suite that needs the embedded binary still gets
        # it. suites.sh has always done it this way: it builds the fixture, exports the
        # TESTDB_* vars, and then explicitly restores the production PATH — "the fixture is
        # for suite subprocesses, not us." This now matches.
        #
        # Address the production store with $SPIRA_BD, never with bare `bd`
        # (law-address-the-store-with-spira-bd).
        FIXTURE_LIB="$WORK/$SPIRA_TESTDB_LIB"
        log "$FAYTH: $BEAD_ID shares one test fixture $TESTDB_NAME (${TESTDB_MODE:-embedded}), built in ${fixture_ms}ms"
    else
        # A FIXTURE THAT WILL NOT BUILD IS NOT A REFUSAL TO WORK. The suites fall back to
        # building their own, which is slow and correct; what must not happen is a bead going
        # unworked because a database server was busy. The reason is logged rather than
        # swallowed, because "slower than it should be" is otherwise invisible.
        # THE WHOLE REASON, FLATTENED AND BOUNDED, not its last lines. The library names what
        # it could not do on its FIRST line and quotes the tool's own output under it, so a
        # tail keeps the detail and drops the diagnosis — and the diagnosis is the half a
        # reader classifies on.
        log "$FAYTH: $BEAD_ID has no shared test fixture — its suites will each build their own: $(tr '\n' ' ' < "$fixture_err" | cut -c1-500)"
    fi
    rm -f "$fixture_err"
fi

# ---- prompt --------------------------------------------------------------------------
# HOW THIS BRANCH LANDS IS PART OF THE BRIEF. An aeon that believes its commit goes straight
# to main writes a different commit from one that knows a reviewer and a CI run stand
# between them, and the harness knows which is true because repo-map says so. Kept to one
# line: brief bloat is compensation for missing context, and this is the context.
case "$REPO_LAND" in
    pr)   LANDING_BRIEF="the sentinel pushes \`$BRANCH\` and opens a pull request against \`$BASE_BRANCH\`; $REPO_NAME's own CI is the gate, so write the commit for a reviewer" ;;
    hold) LANDING_BRIEF="Spira does not advance $REPO_NAME's \`$BASE_BRANCH\`, so the sentinel gates \`$BRANCH\` and leaves it for the operator to merge by hand" ;;
    *)    LANDING_BRIEF="the sentinel merges \`$BRANCH\` into \`$BASE_BRANCH\` and pushes it once the landing gate passes — there is no reviewer between your commit and \`$BASE\`" ;;
esac
# WHETHER THERE IS ANYTHING TO WAIT FOR IS ALSO PART OF THE BRIEF, and the same fact decides
# it. Only `pr` opens a pull request; `push` merges the branch itself and `hold` leaves it for
# a human, so in both of those the landing gate is the only gate and a park waits for an event
# that cannot occur. A gate applied where no run exists is a permanent, invisible hold: not
# claimable, not reported, and shown to the operator as "in CI", the one description that
# stops anybody looking for the real cause.
if [ "$REPO_LAND" = pr ]; then
    PARK_BRIEF="## Your lifetime: do the work, cut the review, then exit

**Do not sit and watch CI.** An Opus session idling for twenty-five minutes while a test
suite runs is the most expensive way to wait that exists. When your work is pushed and its
pull request is open, your job is done for now — exit cleanly and let the harness bring the
bead back when there is something to decide.

What makes that safe is the gate, not your memory of it. Before you exit:

1. push your branch and open or update its pull request
2. create a gh:run gate to park the bead:

   \`\`\`
   GATE_ID=\$(bd gate create --type=gh:run --blocks \$BEAD_ID -r \"waiting for CI\" | grep -oP 'sp-\\w+')
   bd update \$GATE_ID --set-metadata \"repo=\$(gh repo view --json nameWithOwner -q .nameWithOwner)\"
   bd update \$GATE_ID --set-metadata \"branch=\$(git rev-parse --abbrev-ref HEAD)\"
   \`\`\`

3. leave the bead OPEN with a note saying what state it is in

The gate makes the bead not ready — no reader has to remember to exclude a label —
and the harness's gate-check sweep (running every two minutes on spira-gate-check.timer)
finds the matching run via \`bd gate discover\` and resolves the gate via \`bd gate check\`.

Green: the gate resolves, the bead returns to ready, and the sentinel lands it.
Red: \`bd gate check\` escalates the gate; the bead returns to the queue at its own priority
so the next aeon can fix it — same bead, same recorded branch, all your commits.

**Set metadata.repo and metadata.branch** on the gate (step 2 above). The check command
uses metadata.repo to call \`gh run view --repo <org/repo>\`, preventing a run from the
wrong repository from resolving this gate. gate-check's discover step uses metadata.branch
to query only that branch's CI runs, preventing a deployment run on the base branch from
resolving the gate via time proximity."
else
    PARK_BRIEF="## Your lifetime: do the work, then exit

**There is no CI run to wait for in this repository.** $REPO_NAME lands by \`$REPO_LAND\`, so
nothing opens a pull request for \`$BRANCH\` and no run will ever report on it. The landing
gate is the only gate, and once it passes there is nothing further to wait for.

**So do not create a gh:run gate for this bead.** A gate means \"parked on a run somebody
else is watching\", and it excludes the bead from ready — the mechanism that would otherwise
make the bead claimable. Applied where no run exists it is a permanent, invisible hold: not
claimable, not reported, and shown to the operator as \"in CI\", which is the one description
that stops anybody looking for the real cause.

When the work is committed on your branch, close the bead with its evidence and exit. How the
branch reaches \`$BASE_BRANCH\` from there is described above, and none of it needs you."
fi

# WHAT THIS SESSION IS ACTUALLY HOLDING. The brief is read by aeons working every repository
# and most of them have no fixture library at all, so a persona that stated flatly "your
# fixture is already built" would be wrong more often than right — and an instruction that is
# visibly false about something checkable is a reason to distrust the rest of the brief.
if [ -n "$FIXTURE_LIB" ]; then
    if [ "${TESTDB_MODE:-embedded}" = server ]; then
        _fixture_engine="the dolt-beads-test server (port ${SPIRA_TESTDB_PORT:-3308})"
        _fixture_cleanup="testdb_drop (which stops dolt-beads-test.service if this session started it)"
    else
        _fixture_engine="the embedded Dolt engine — no shared server, no external port"
        _fixture_cleanup="\`rm -rf\` on the fixture directory"
    fi
    FIXTURE_BRIEF="**The fixture is already built.** One throwaway database was created for this
session and exported into your environment (\`TESTDB_SHARED=1\`, \`TESTDB_NAME=$TESTDB_NAME\`),
so a suite that sources \`$SPIRA_TESTDB_LIB\` and calls \`testdb_up\` resets it in a fraction of
a second instead of spending the ${fixture_ms}ms that build cost. Never unset those variables
and never build a database of your own: a suite that reaches past \`testdb_up\` pays the build
again on every run, and nothing anywhere reports that it did.

The fixture uses $_fixture_engine. Cleanup is $_fixture_cleanup."
else
    FIXTURE_BRIEF="This repository has no shared test fixture, so a suite that needs one builds
its own. If that turns out to be the slowest thing in your session, say so when you close the
bead — the number is worth having."
fi

# HOW TO VERIFY HAS EXACTLY ONE SOURCE: the persona's own Tests section in chamber/$FAYTH.md.
# A second, hard-coded instruction rendered from here duplicated it and drifted out of sync
# with it — a brief that says two different things about the same question is worse than one
# that says nothing.
#
# ---- the deadline ---------------------------------------------------------------------
# A SESSION THAT CAN BE KILLED MUST BE ABLE TO SEE WHEN. A persona that declares
# FAYTH_TIMEOUT_SECONDS is killed from outside — the transient unit's TimeoutStartSec is set
# from that same key — and its brief then asks it, if it cannot finish, to leave what it
# found in the graph rather than in a session that is about to end. It had no way to tell how
# long that was. Measured: four consecutive sessions on one incident, every one killed within
# a second of the wall, and no commit and no bead between them; each had found something and
# each took it with it. The wall is not the defect. Not being able to see it is.
#
# ANCHORED AT AEON_T0, THIS SCRIPT'S OWN START. The wall is on the unit, and by the time the
# prompt is rendered the unit has already spent seconds claiming a bead and building a
# worktree — a countdown started here would be exactly that much too generous, and the number
# the aeon needs is the one it can still spend. Run by hand with no unit around it, only the
# `timeout` on the session below applies and that starts later still, so this reads early
# rather than late; early is the harmless direction.
#
# THE EPOCH IS IN THE TEXT ON PURPOSE. A clock time says when the session dies; the epoch is
# what lets it ASK how much is left, at any point, in one command that depends on nothing
# here. An aeon that has to estimate its remaining time will estimate it generously.
if [ -n "${FAYTH_TIMEOUT_SECONDS:-}" ]; then
    DEADLINE_AT=$(( AEON_T0 + FAYTH_TIMEOUT_SECONDS ))
else
    DEADLINE_AT=""
fi
DEADLINE_BRIEF="$(render_deadline_brief "$DEADLINE_AT" "$(date +%s)")"

# ---- chamber overlay: an operator's own copy of a brief outlives a release --------------
# A hand edit made straight into the release checkout is reverted by the next skew refresh
# with nothing to say so happened (sp-r1ca2). Read order, first hit wins per layer:
#   $FAYTH.md            in SPIRA_CHAMBER_OVERLAY replaces the release brief whole
#   $FAYTH.<section>.md  replaces one "## <section>" block of it; the file's own first line
#                        must be that exact heading, so a rename is visible in the file
#                        rather than silent, and a section named that is not in the release
#                        brief is logged and ignored rather than applied nowhere
#   $FAYTH.append.md     appended after everything else
# {{PLACEHOLDER}} substitution below runs on the combined result, so an overlay may use
# every token the release brief can.
CHAMBER_FILE="$SPIRA_HOME/chamber/$FAYTH.md"
CHAMBER_OVERLAY_WHOLE="$SPIRA_CHAMBER_OVERLAY/$FAYTH.md"
CHAMBER_OVERLAY_APPEND="$SPIRA_CHAMBER_OVERLAY/$FAYTH.append.md"
if [ -f "$CHAMBER_OVERLAY_WHOLE" ]; then
    CHAMBER_CONTENT="$(cat "$CHAMBER_OVERLAY_WHOLE")"
    log "$FAYTH: $BEAD_ID chamber brief replaced whole-file by $CHAMBER_OVERLAY_WHOLE"
else
    CHAMBER_CONTENT="$(cat "$CHAMBER_FILE" 2>/dev/null)"
    for _co_f in "$SPIRA_CHAMBER_OVERLAY/$FAYTH".*.md; do
        [ -f "$_co_f" ] || continue
        [ "$_co_f" = "$CHAMBER_OVERLAY_APPEND" ] && continue
        _co_section="${_co_f#"$SPIRA_CHAMBER_OVERLAY/$FAYTH".}"; _co_section="${_co_section%.md}"
        _co_heading="## ${_co_section//_/ }"
        if grep -qxF "$_co_heading" <<<"$CHAMBER_CONTENT"; then
            CHAMBER_CONTENT="$(awk -v h="$_co_heading" -v rf="$_co_f" '
                BEGIN { while ((getline line < rf) > 0) repl = repl line "\n"; close(rf) }
                $0 == h { printf "%s", repl; skip=1; next }
                skip && /^## / { skip=0 }
                skip { next }
                { print }
            ' <<<"$CHAMBER_CONTENT")"
            log "$FAYTH: $BEAD_ID chamber section '$_co_heading' overlaid by $_co_f"
        else
            log "$FAYTH: $BEAD_ID chamber overlay $_co_f names a section ($_co_heading) absent from $CHAMBER_FILE — ignored"
        fi
    done
    [ -f "$CHAMBER_OVERLAY_APPEND" ] && CHAMBER_CONTENT="$(printf '%s\n\n%s' "$CHAMBER_CONTENT" "$(cat "$CHAMBER_OVERLAY_APPEND")")"
fi

# THE INJECTED BLOCKS ARE OVERLAID THE SAME WAY, under SPIRA_CHAMBER_OVERLAY/blocks/ rather
# than beside the persona file, because they are text THIS SCRIPT computes (from repo-map and
# the fixture, not from a chamber/*.md file) and are shared across personas. Unlike the
# section overlay above, this text is spliced in after the {{PLACEHOLDER}} pass runs (the
# reason PARK/FIXTURE/DEADLINE are parameter-expanded rather than sed'd in the first place —
# see the comment at their substitution below), so a block overlay file is used verbatim; it
# cannot itself contain a {{TOKEN}} and expect it resolved.
block_overlay() {   # block_overlay <name> <built-in text> -> stdout
    local f="$SPIRA_CHAMBER_OVERLAY/blocks/$1.md"
    if [ -f "$f" ]; then
        log "$FAYTH: $BEAD_ID $1 block overlaid by $f"
        cat "$f"
    else
        printf '%s' "$2"
    fi
}
PARK_BRIEF="$(block_overlay PARK "$PARK_BRIEF")"
FIXTURE_BRIEF="$(block_overlay FIXTURE "$FIXTURE_BRIEF")"
DEADLINE_BRIEF="$(block_overlay DEADLINE "$DEADLINE_BRIEF")"

BEAD_BODY="$(bdq show "$BEAD_ID" 2>/dev/null | grep -vE '^💡|^warning|^  Fix|^  Or')"
# A THRASHED BEAD'S BRIEF LEADS WITH THE STICKING POINT, ahead of the "## The bead" heading
# itself — not folded into {{BEAD}}, which lands AFTER that heading, still above a bare bead
# body the aeon would otherwise have to scroll a note history to find it in. Only when this
# session's own worktree tip still matches the tip recorded at the last thrash — a moved tip
# means the sticking point is already stale, and repeating it would waste the turn it was
# meant to save.
_thrash_banner=""
_thrash_meta_streak="$(bead_metadata "$BEAD_ID" thrash_streak)"
if [ -n "$_thrash_meta_streak" ] && [ "$_thrash_meta_streak" -ge 1 ] 2>/dev/null; then
    _thrash_meta_tip="$(bead_metadata "$BEAD_ID" thrash_tip)"
    _thrash_cur_tip="$(git -C "$WORK" rev-parse --short HEAD 2>/dev/null || echo ?)"
    if [ -n "$_thrash_meta_tip" ] && [ "$_thrash_meta_tip" = "$_thrash_cur_tip" ]; then
        _thrash_meta_last="$(bead_metadata "$BEAD_ID" thrash_last)"
        _thrash_banner="STICKING POINT ($_thrash_meta_streak consecutive thrash(es), nothing committed since): ${_thrash_meta_last:-?}
Start there — do not spend a turn rediscovering it from the note history below.
"
    fi
fi
PROMPT="$(sed -e "s|{{BEAD_ID}}|$BEAD_ID|g" -e "s|{{BRANCH}}|$BRANCH|g" \
              -e "s|{{REPO}}|$WORK|g" -e "s|{{REPO_NAME}}|$REPO_NAME|g" \
              -e "s|{{HOME_REPO}}|$(spira_home_repo)|g" \
              -e "s|{{LANDING}}|$LANDING_BRIEF|g" -e "s|{{DB}}|$SPIRA_DB|g" \
              -e "s|{{SPIKE_DIR}}|$SPIRA_SPIKE_DIR|g" -e "s|{{SPIKE_PATHS}}|$SPIRA_SPIKE_PATHS|g" \
              -e "s|{{SOP}}|$SPIRA_HOME/sop.sh|g" -e "s|{{INCIDENT}}|$SPIRA_HOME/incident.sh|g" \
              -e "s|{{ASK}}|$SPIRA_HOME/mail.sh|g" -e "s|{{SUITES}}|$SPIRA_HOME/suites.sh|g" \
              -e "s|{{GROOM}}|$SPIRA_HOME/groomer.sh|g" \
              -e "s|{{SPIRA_HOME}}|$SPIRA_HOME|g" \
              -e "s|{{RUN}}|$SPIRA_RUN|g" \
              -e "s|{{MAX_BEADS}}|$SPIRA_MAECHEN_MAX_BEADS|g" \
              -e "s|{{REMEDY_LABEL}}|$SPIRA_MAECHEN_REMEDY_LABEL|g" \
              -e "s|{{SCOPE}}|${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}|g" \
              <<<"$CHAMBER_CONTENT")"
# PARAMETER EXPANSION, NOT sed, for the multi-line substitutions. `s|{{X}}|<many lines>|`
# is not a thing sed will do, and a brief that silently rendered as the literal `{{PARK}}`
# would leave an aeon with no instruction at all about how its work is meant to end.
PROMPT="${PROMPT/\{\{BEAD\}\}/$BEAD_BODY}"
PROMPT="${PROMPT/\{\{PARK\}\}/$PARK_BRIEF}"
PROMPT="${PROMPT/\{\{FIXTURE\}\}/$FIXTURE_BRIEF}"
PROMPT="${PROMPT/\{\{DEADLINE\}\}/$DEADLINE_BRIEF}"
# The banner belongs in task.md, not system.md — system_prompt_split (lib.sh) cuts PROMPT at
# the FIRST "<!-- task -->" marker, so a plain prepend to PROMPT would land the banner in the
# system half. Insert it just after that marker instead; a persona with no marker puts its
# whole prompt in task.md anyway, so prepending is equivalent there.
if [ -n "$_thrash_banner" ]; then
    if [[ "$PROMPT" == *'<!-- task -->'* ]]; then
        PROMPT="${PROMPT/<!-- task -->/<!-- task -->
$_thrash_banner}"
    else
        PROMPT="$_thrash_banner
$PROMPT"
    fi
fi

# The memory book. Every agent reads it on every session; this is the delivery mechanism
# for an aeon, standing in for the SessionStart hook an interactive session gets.
#
# WHICH book is the fayth's to declare. Statutes (`law-`) are how to behave and everyone
# reads them; SOPs (`sop-`) are how to fix and only Ops executes one. Without the filter a
# growing shelf of runbooks would be charged to every builder session and would eventually
# crowd out the law itself.
#
# This used to be `bd memories | head -400`, which was wrong twice over: `bd memories` is a
# LISTING and truncates every body at ~110 characters, so aeons have been reading
# half-sentences of the law they are held to, and the `head` then dropped whichever
# memories sorted last without saying so. render_memories reads the JSON and prints each
# one whole.
STATUTES="$(render_memories "${FAYTH_MEMORY_PREFIXES:-law-}" "" "${FAYTH_STATUTE_CORE:-}")"
# THE MACHINE-READABLE ALTERNATIVE TO "ALREADY DONE" IS PART OF THE BRIEF. An aeon that
# concludes the work is already done will close with "already done" in the reason unless it
# is explicitly told not to. The sentinel reads the COMMIT GRAPH, not the close reason: a
# bare close without a commit naming the bead is indistinguishable from a failed attempt and
# is reopened with an attempt charged toward the poison threshold. Two attempts that way is
# one from poison. `bd supersede` records the relation where the sentinel, landing pass, and
# cleanup checks all read it; a close reason is read by none of them.
#
# THE SUCCESSOR MUST BE VERIFIED AS LANDED BEFORE `bd supersede` IS RUN. Closed is not
# landed: a bead can be closed without its commit on the base, so a supersede decision made
# from the successor's status alone retires this bead against a promise that may never be
# kept and nothing downstream will notice the gap.
ALREADY_DONE_BRIEF="## If you find the work is already done

If you conclude this bead's work has already landed on \`$BASE\` under another commit — a
different bead already carried it — do **not** close with an \"already done\" reason. The
sentinel verifies landing by reading the commit graph, not the close reason: a bare close
without a commit naming \`$BEAD_ID\` is indistinguishable from a failed attempt, and the
sentinel reopens it and charges an attempt toward the poison threshold.

The machine-readable path:

1. **Verify the successor actually landed.** Closed is not landed — a bead can be closed
   without its commit on the base. Check the commit graph, not the bead's status:

       git -C $WORK log --format='%s' -n \${SPIRA_VERDICT_WINDOW:-400} $BASE | grep <successor-id>

2. Once confirmed on the base, **run \`bd supersede\`**:

       bd -C $SPIRA_DB supersede $BEAD_ID --with <successor-id>

That records the relation so the sentinel, landing pass, and cleanup checks all recognise this
bead as retired and skip it correctly. A close reason alone is not read by any of them."

# THE LAST STEP BEFORE CLOSING IS A REBASE, AND IT IS THE AEON'S. The landing pass rebases
# too, but it cannot resolve a conflict — it reopens the bead and hands the conflict to the
# NEXT aeon, which arrives with none of the context that wrote the commits. With several
# aeons landing, the base moves between an aeon's close and its landing by construction, and
# 12 of the first 23 reopens this harness performed were exactly that. The session that
# holds the context is the one that should pay for the conflict, so it is told to, and the
# verdict step below checks that it did.
CLOSE_BRIEF="## Before you close: rebase onto \`$BASE\`

Other aeons land while you work, so \`$BASE\` has probably moved. The last thing you do
before closing the bead — after your commits, before the close — is:

    git -C $WORK fetch ${BASE_REMOTE:-origin}
    git -C $WORK rebase $BASE

Resolve any conflict yourself: you wrote these commits and you know what they mean, and the
landing pass does not — it would reopen the bead and hand the conflict to a stranger. Then
run the gate once more on the rebased tree, and close. A bead closed behind \`$BASE\` that
does not rebase cleanly is reopened by the harness, which costs a whole second session."

# Split the persona prompt on <!-- task --> into system and task layers.
# system.md: persona identity, standing rules, statutes.
# task.md: bead body, deadline, session-specific briefs.
SYSTEM_FILE="$SPIRA_RUN/$BEAD_ID.system.md"
TASK_FILE="$SPIRA_RUN/$BEAD_ID.task.md"
system_prompt_split "$SYSTEM_FILE" "$TASK_FILE" "$STATUTES" "$PROMPT"
# Append session-specific briefs to the task file.
{
    printf '%s' "$DIRTY_BRIEF"
    printf '%s' "$RESUME_BRIEF"
    printf '%s' "$SLAIN_BRIEF"
    printf '%s' "$ALREADY_DONE_BRIEF"
    printf '%s' "$CLOSE_BRIEF"
    printf '%s' "$REBASE_BRIEF"
} >> "$TASK_FILE"

# ---- the shelf, before ----------------------------------------------------------------
# READ BEFORE THE SESSION RUNS, for the closing-rule check at the foot of this script. A
# write to the shelf is a CHANGE and there is no way to see one after the fact: `bd remember`
# upserts, so amending an existing runbook leaves a shelf of exactly the size and shape it
# had. Two digests and a comparison is the whole mechanism.
#
# ONLY FOR A PERSONA THAT DECLARES THE RULE, so no builder session pays a `bd memories`
# query for a check that will not run.
#
# NOTHING ELSE IS WRITING THE SHELF BETWEEN THESE TWO READS: the persona that holds this
# rule has one lane by construction (FAYTH_MAX_CONCURRENT=1 and a party member, so it is not
# drawn from the aeon pool). If a second concurrent writer is ever introduced the comparison
# widens rather than narrows — it would see the other session's write and let this one pass,
# which is the harmless direction.
#
# THE EPOCH IS TAKEN FROM THE SAME CLOCK the applications ledger stamps its records with
# (`date -u +%s`), because the second half of the check asks whether THIS session recorded
# anything, not whether the bead has ever been recorded against. An incident reopened after
# an earlier session did the honest thing would otherwise hand a later silent session a pass.
SOP_REQUIRED="${FAYTH_SOP_REQUIRED:-0}"
SHELF_BEFORE=""; SHELF_BEFORE_OK=0; SESSION_EPOCH="$(date -u +%s)"
if [ "$SOP_REQUIRED" = 1 ]; then
    # THE INSTRUMENT BEFORE ITS SILENCE IS BELIEVED. The applications ledger reads as
    # UNREADABLE when the file does not exist — correctly, because from inside sop.sh an
    # absent file and a misconfigured path are the same observation. On a fresh install
    # nothing has ever created it, so without this the check would decline to judge every
    # incident until the first honest session happened to record one, and the very sessions
    # it exists to catch would be exactly the ones it never saw. An empty ledger is a real
    # ledger: with it in place, "read it, nothing there" becomes an answer that can be had.
    "$SPIRA_HOME/sop.sh" ledger-init >/dev/null 2>&1 \
        || log "$FAYTH: could not create the SOP applications ledger — the closing rule cannot be judged this run"
    if SHELF_BEFORE="$("$SPIRA_HOME/sop.sh" digest 2>/dev/null)"; then
        SHELF_BEFORE_OK=1
    else
        # SAID OUT LOUD AND NOT SWALLOWED. An unreadable shelf here is what makes the check
        # decline to judge later, and a silent decline is indistinguishable from a check
        # that ran and found nothing wrong.
        log "$FAYTH: could not read the SOP shelf before the session — the closing rule cannot be judged this run"
    fi
fi

# ---- the groom log, before ------------------------------------------------------------
# Line count captured before the session runs. Lines beyond this count after the session
# are the new lines written by this pass — the window the escalation check reads.
# ONLY FOR A PERSONA THAT DECLARES FAYTH_GROOM_ESCALATION_CHECK=1.
GROOM_ESCALATION_CHECK="${FAYTH_GROOM_ESCALATION_CHECK:-0}"
GROOM_LOG_LINES_BEFORE=0
if [ "$GROOM_ESCALATION_CHECK" = 1 ]; then
    _groom_log_path="${SPIRA_RUN}/groom.log"
    [ -f "$_groom_log_path" ] && GROOM_LOG_LINES_BEFORE="$(wc -l < "$_groom_log_path" 2>/dev/null)" || true
fi

# ---- work ----------------------------------------------------------------------------
# APPEND, NEVER TRUNCATE — see attempt_trace in lib.sh. A `>` here erased the previous
# attempt's trace, so a bead only ever had a record of its last session; the mark line
# written when this attempt began is what lets every reader still find where the last
# session starts.
log "$FAYTH: working $BEAD_ID on $BRANCH (log: $LOGF)"
set +e
cd "$WORK" || die "worktree missing: $WORK"
# No `timeout` here. A backstop remains available as FAYTH_TIMEOUT_SECONDS for a fayth that
# genuinely wants one, but it is unset by default: the stall detector above is what ends a
# wedged session, and it ends it by ceasing to heartbeat rather than by killing work that
# might be nearly done.
# STREAM THE SESSION: the log is a live trace, not a buffered dump (the operator, verbatim:
# "one way to gauge liveness of a claude session is to ensure it launches with full tracing
# and then watch the trace"). With the default text format nothing reaches the log until
# the session ends — a run was observed at 0 bytes eight minutes in — so the log could not
# answer "is it working". stream-json emits an event per message and per tool use, which
# makes the log the authoritative progress signal and lets the heartbeat stop guessing from
# CPU ticks and file mtimes.
# THE BINARY IS INJECTABLE, like `bd`, `gh` and `systemd-run` before it, and for the same
# reason: it is the one thing a test of this path must be able to replace. And a PATH shim
# CANNOT do it — conf.sh REPLACES $PATH outright a few lines into this script, so a suite
# that puts a fake first on PATH runs the real model against the operator's account,
# silently and at full cost. That is not hypothetical; it is how this line came to be
# written. A test overrides SPIRA_AGENT.
_AEON_GH_EMPTY="$SPIRA_RUN/aeon-empty-gh"
mkdir -p "$_AEON_GH_EMPTY" 2>/dev/null || true
export GH_CONFIG_DIR="$_AEON_GH_EMPTY"
unset GH_TOKEN GITHUB_TOKEN
export GIT_SSH_COMMAND="echo 'aeon: no SSH credentials — landing.sh and batch.sh handle forge writes' >&2; exit 1"
export GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=/bin/false
mapfile -t _AEON_ARGV < <(aeon_claude_argv "$SPIRA_SYSTEM_FLAG" "$SYSTEM_FILE")
SESSION_STARTED=1
cat "$TASK_FILE" | ${FAYTH_TIMEOUT_SECONDS:+timeout $FAYTH_TIMEOUT_SECONDS} \
    "${SPIRA_AGENT:-claude}" "${_AEON_ARGV[@]}" \
    >> "$LOGF" 2>&1
rc=$?
SESSION_RC=$rc   # held for cleanup, which sees only $? at the time the trap fires
set -e
log "$FAYTH: $BEAD_ID session exited rc=$rc"

# ---- wiki checkout commit -----------------------------------------------------------
# An aeon that writes to the brain wiki must commit before it exits. The brain
# checkout is shared; uncommitted writes are invisible to the commit graph and
# at risk of being lost or mis-attributed to the next session that touches it.
#
# ONLY PATHS THIS SESSION'S OWN TOOL CALLS NAMED, per wiki_write_paths (lib.sh) reading the
# session's own transcript — never a before/after dirty-snapshot diff, which cannot exclude
# a file another actor dirties after the snapshot is taken (sp-4fl2e). Intersected with what
# is actually dirty now, since a path the session wrote and then reverted, or that another
# writer already committed, has nothing left to stage. wiki/tasks.md is a generated view
# regenerated by the brain session's SessionStart hook; it is not authored by aeons and is
# excluded.
if [ -n "${SPIRA_WIKI:-}" ] && [ -d "$SPIRA_WIKI" ]; then
    _wc_main="$(git -C "$SPIRA_WIKI" worktree list --porcelain 2>/dev/null \
                  | awk '/^worktree /{print $2; exit}')"
    _wc_real="$(cd "$SPIRA_WIKI" 2>/dev/null && pwd -P)"
    if [ -n "${_wc_main:-}" ] && [ "${_wc_real:-}" = "$_wc_main" ]; then
        _wc_dirty="$(git -C "$SPIRA_WIKI" status --short --untracked-files=all 2>/dev/null | cut -c4- | sort -u)"
        _wc_new="$(wiki_commit_paths "$(wiki_write_paths "$LOGF" "$SPIRA_WIKI")" "$_wc_dirty")"
        if [ -n "$_wc_new" ]; then
            _wc_count="$(printf '%s\n' "$_wc_new" | grep -c .)"
            if printf '%s\n' "$_wc_new" | bash "$SPIRA_HOME/wiki-commit.sh" \
                    "$SPIRA_WIKI" "$FAYTH: wiki writes for $BEAD_ID"; then
                log "$FAYTH: $BEAD_ID committed wiki changes ($_wc_count file(s)): $(printf '%s' "$_wc_new" | head -5 | tr '\n' ' ')"
            else
                log "$FAYTH: $BEAD_ID wiki commit failed"
            fi
            unset _wc_count
        fi
    fi
    unset _wc_main _wc_real _wc_new _wc_dirty
fi

# ---- verdict -------------------------------------------------------------------------
# Closed is not landed. The aeon may have closed the bead; that claim is only believed if
# a commit on its branch or on the landing refs actually names the bead id.
# ONE `bd show`, TWO FACTS — the status AND whether the bead was superseded. Status alone
# cannot judge the close, because A SUPERSEDED BEAD WILL NEVER HAVE A COMMIT NAMING IT: its
# work was carried onto the successor's branch and landed under the successor's id. Reopening
# it says "closed without landing" about work that is already on the base branch, and since
# the next summon re-cuts the branch and runs a whole session against a duplicate, the bead
# cycles forever. One did, five times over, after the identical exemption was added to the
# sentinel's closed-but-not-landed check and not to this one — the two ask the same question
# and must answer it the same way.
#
# `bd list` AND `bd show` NAME THE SAME FIELD DIFFERENTLY: show returns "dependency_type",
# list returns "type". Accept either spelling rather than the one this call happens to
# return, because nothing here can tell which shape it was handed.
verdict="$(bdjson show "$BEAD_ID" 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: print("\t0\t"); sys.exit()
d=d if isinstance(d,list) else [d]
if not d: print("\t0\t"); sys.exit()
sup = 1 if any((x.get("dependency_type") or x.get("type")) == "supersedes"
               for x in (d[0].get("dependencies") or [])) else 0
# delivers:TYPE labels stamped by incident.sh on filing — the aeon verifies each here
# (and the sentinel re-checks in CHECK5) so a closed bead with satisfied evidence is not
# reopened for lacking a commit.
lab = d[0].get("labels") or []
delivers = ";".join(l[len("delivers:"):] for l in lab if l.startswith("delivers:"))
print("%s\t%s\t%s" % (d[0].get("status",""), sup, delivers))' 2>/dev/null)"
st="${verdict%%	*}"; _vrest="${verdict#*	}"; superseded="${_vrest%%	*}"; delivers="${_vrest#*	}"
# NEVER `git log | grep -q` under `set -o pipefail`. grep -q exits on the first match and
# closes the pipe; git log then dies of SIGPIPE and pipefail propagates 141 as the
# pipeline's status, so a MATCH reads as a failure. This exact line reported "closed with
# nothing committed" about sp-epic-complete, whose commit was already on the branch, and
# reopened finished work. Capture first, match second.
#
# CHECK THE BRANCH, THEN THE LANDING REFS. Two earlier defects in this check:
#
#   1. The branch was checked with -n 50 while the sentinel's CHECK5 walks the same question
#      with -n 400 (SPIRA_VERDICT_WINDOW). A bead whose commit sits 51+ commits back on the
#      base was missed by aeon.sh and correctly seen by CHECK5, so aeon.sh reopened what
#      CHECK5 left alone and the bead cycled. sp-jll: landed 113 commits behind origin/main,
#      every subsequent session spent rediscovering that the work was done.
#
#   2. The branch was checked, not the landing refs. When a branch carries leftover commits
#      from a previous attempt (rebased onto the new base), the branch tip is those commits +
#      the base history: the bead's commit on the base appears deeper from the branch tip than
#      from the base tip, and a bounded walk can find it via the base but not the branch. The
#      sentinel walks spira_landrefs, not the branch, and the two must not disagree.
#
# Walk the branch first (covers commits from the CURRENT session not yet on the base),
# then the landing refs (covers commits already on the base). The window is the same in
# both — SPIRA_VERDICT_WINDOW — to match sentinel CHECK5 and landed() in lib.sh.
committed="$(verdict_committed "$REPO" "$BRANCH" "$BEAD_ID")"
log "$FAYTH: $BEAD_ID status=$st committed=$committed superseded=$superseded delivers=${delivers:-none}"

# EVICTION RACE. eviction_reopen (lib.sh) decides reopen/stale/cap/none from the landstate
# record, the current branch tip and the prior eviction-race requeue count; this block is
# the side effects (idempotence sidecar, the reopen/escalate calls, the log lines).
if [ "$st" = "closed" ] && [ "$committed" = "yes" ] && [ "$superseded" != 1 ]; then
    _evict_ls="$(land_state "$BEAD_ID" 2>/dev/null)" || _evict_ls=""
    if [ -n "$_evict_ls" ] && [ "$(eviction_reopen "$_evict_ls" "" 0)" != none ]; then
        _evict_state="" _evict_tip="" _evict_at="" _evict_reason=""
        read -r _evict_state _evict_tip _evict_at _evict_reason <<< "$_evict_ls"
        _evict_seen_f="$LANDSTATE/$BEAD_ID.evict-seen"
        _evict_seen="$(cat "$_evict_seen_f" 2>/dev/null)" || _evict_seen=""
        if [ -n "$_evict_seen" ] && [ "$_evict_seen" = "$_evict_tip $_evict_reason" ]; then
            log "$FAYTH: $BEAD_ID closed with landstate=$_evict_state — tip+reason unchanged since the last eviction-race reopen, duplicate skipped"
        else
            _cur_tip="$(git -C "$REPO" rev-parse "$BRANCH" 2>/dev/null)" || _cur_tip=""
            _evict_count="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
                "SELECT COUNT(*) FROM events WHERE issue_id='$BEAD_ID' AND event_type='requeued' AND new_value='eviction-race'" \
                2>/dev/null | sed -n '3p' | tr -d ' ')" || _evict_count=0
            _evict_count="${_evict_count:-0}"
            case "$(eviction_reopen "$_evict_ls" "$_cur_tip" "$_evict_count")" in
            stale)
                log "$FAYTH: $BEAD_ID closed with landstate=$_evict_state — record tip $_evict_tip ≠ branch tip $_cur_tip, stale record, close stands"
                ;;
            cap)
                bdq label add "$BEAD_ID" "$SPIRA_ASK_LABEL" >/dev/null 2>&1 || true
                bdq note "$BEAD_ID" "Eviction-race guard capped: reopened $_evict_count time(s) already. Recertify the branch by hand and clear the $SPIRA_ASK_LABEL label; the guard will not reopen it again on its own." >/dev/null 2>&1 || true
                log "$FAYTH: $BEAD_ID eviction-race escalated — $_evict_count prior requeue(s) ≥ ${SPIRA_EVICTION_ESCALATE_AT:-3}, labeled $SPIRA_ASK_LABEL instead of reopening"
                printf '%s %s' "$_evict_tip" "$_evict_reason" > "$_evict_seen_f" 2>/dev/null || true
                ;;
            reopen)
                bead_reopen "$BEAD_ID" eviction-race "Reopened by aeon.sh: bead closed while landstate is $_evict_state — the branch was evicted from the batch while this session was in flight. The close is valid but the work cannot re-enter the queue while the bead is closed. Recertify the branch to re-enter the merge queue."
                log "$FAYTH: $BEAD_ID REOPENED — closed with landstate=$_evict_state (eviction race)"
                st="open"
                REQUEUE_CAUSE="eviction-race"
                REQUEUE_WHY="Batch evicted the branch while this aeon was in flight; the bead was re-closed on a stale pass. Recertify the branch."
                printf '%s %s' "$_evict_tip" "$_evict_reason" > "$_evict_seen_f" 2>/dev/null || true
                ;;
            esac
            unset _cur_tip _evict_count
        fi
        unset _evict_seen _evict_seen_f
    fi
    unset _evict_ls _evict_state _evict_tip _evict_at _evict_reason
fi

# close_verdict/delivers_verdict (lib.sh) decide identically for aeon.sh and sentinel
# CHECK5 (UC-aeon-execution-13) — SESSION_EPOCH is the lower bound for file mtime: a file
# written before this session does not count as delivers:note/report evidence.
_cv="$(close_verdict "$BEAD_ID" "$st" "$superseded" "${delivers:-}" "$committed" "${SESSION_EPOCH:-0}")"
_cv_outcome="${_cv%%|*}"; _cv_rest="${_cv#*|}"; _cv_reason="${_cv_rest%%|*}"; _cv_msg="${_cv_rest#*|}"
case "$_cv_outcome|$_cv_reason" in
keep\|delivers)
    # SAID OUT LOUD. A silent decline is indistinguishable from the check never running.
    log "$FAYTH: $BEAD_ID closed with nothing committed and NOT reopened — $_cv_msg"
    ;;
keep\|superseded)
    # SAID OUT LOUD. This is the one path where the harness sees a bead closed with nothing
    # committed and declines to act, and a silent decline is indistinguishable from the
    # check never having run at all.
    log "$FAYTH: $BEAD_ID closed with nothing committed and NOT reopened — superseded, so its work landed under another id"
    ;;
reopen\|delivers-mismatch)
    _producer="$(bdjson show "$BEAD_ID" 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("created_by","") or "")
except Exception: print("")' 2>/dev/null)"
    _producer_msg=""
    [ -n "$_producer" ] && _producer_msg=" Escalate to $_producer if the criterion cannot be met — the delivers: label may not be removed."
    bead_reopen "$BEAD_ID" delivers-mismatch "Reopened by aeon.sh: $_cv_msg. Set delivers:TYPE labels that match the evidence actually produced.${_producer_msg}"
    log "$FAYTH: $BEAD_ID REOPENED — delivers not verified: $_cv_msg"
    ;;
reopen\|closed-without-commit)
    bead_reopen "$BEAD_ID" closed-without-commit "Reopened by aeon.sh: closed without a commit naming $BEAD_ID on $BRANCH. Closed is not landed."
    log "$FAYTH: $BEAD_ID REOPENED — closed with nothing committed"
    ;;
esac

# ---- own-worktree dirty guard -----------------------------------------------
# The aeon's own worktree ($WORK) must be clean when the bead is closed. A bead closed while
# the worktree carries uncommitted tracked modifications (staged but not committed) means
# unreviewed code is invisible to the commit graph — committed is not staged.
#
# BOUND TO $WORK, NOT $SPIRA_REPO. Binding this guard to the shared harness checkout punishes
# whichever aeon happens to close next for a condition it neither caused nor can fix, producing
# an unbounded requeue loop whenever any stray file appears in the shared checkout. $WORK is
# the aeon's own tree — it is the one thing this aeon wrote, and it is the right scope.
#
# When a modified path is byte-for-byte identical to the landing ref (content hand-applied
# rather than pulled), git checkout -- <path> is the one-command remedy.
#
# DOES NOT FIRE ON A BEAD THAT WAS REOPENED ABOVE. st="open" from the no-commit check or
# the delivers check means the guard below is skipped: the bead is already open, and
# stacking a second reopen on top of the first would leave contradicting notes on the same
# re-opening event.
#
# SPIRA_ALLOW_PROD_DIRTY=1 overrides — the fence is polite, not a wall.
if [ "$st" = "closed" ] && [ "$committed" = "yes" ] && [ -z "${SPIRA_ALLOW_PROD_DIRTY:-}" ]; then
    _spd_dirty="$(git -C "$WORK" status --porcelain --untracked-files=no 2>/dev/null)" || true
    if [ -n "$_spd_dirty" ]; then
        # Walk the dirty-vs-HEAD names and compare each against the landing ref rather than
        # HEAD. A worktree that is BEHIND and DIRTY understates the delta if measured against
        # HEAD alone: the remote ref is the authoritative version in force.
        _spd_base="$(spira_landref "$WORK" 2>/dev/null \
            || git -C "$WORK" rev-parse --abbrev-ref HEAD 2>/dev/null \
            || printf 'HEAD')"
        _spd_identical="" _spd_path=""
        while IFS= read -r _spd_path; do
            [ -n "$_spd_path" ] || continue
            # diff --quiet exits 0 when the path is identical — no differences found.
            if git -C "$WORK" diff --quiet "$_spd_base" -- "$_spd_path" 2>/dev/null; then
                _spd_identical="${_spd_identical:+$_spd_identical }$_spd_path"
            fi
        done < <(git -C "$WORK" diff --name-only HEAD 2>/dev/null)

        _spd_note="Reopened by aeon.sh: bead closed while the aeon's own worktree ($WORK) carried uncommitted tracked modifications. Staged but uncommitted code is invisible to the commit graph — commit it or restore the file.

Modified paths:
$(printf '%s\n' "$_spd_dirty" | sed 's/^/  /')"
        if [ -n "$_spd_identical" ]; then
            _spd_note="$_spd_note

Paths byte-for-byte identical to $_spd_base (hand-applied, not genuinely new):
  $_spd_identical
Remedy: git -C $WORK checkout -- $_spd_identical"
        fi
        _spd_note="$_spd_note

Override (only when the modification is intentional and will be committed separately): SPIRA_ALLOW_PROD_DIRTY=1"

        bead_reopen "$BEAD_ID" prod-dirty "$_spd_note"
        # PREVENT DOUBLE-FIRING. The SOP check and rebase check below both test [ st=closed ].
        # Setting st here skips them: the bead is already reopened, and re-running those checks
        # against a bead this process just put back would produce contradicting notes.
        st="open"
        log "$FAYTH: $BEAD_ID REOPENED — own worktree dirty: $(git -C "$WORK" diff --name-only HEAD 2>/dev/null | head -5 | tr '\n' ' ')"
        REQUEUE_CAUSE="prod-dirty"
        REQUEUE_WHY="Bead closed while the aeon's own worktree ($WORK) carried uncommitted tracked modifications. Commit or restore the staged/modified files, then resume this bead."
        unset _spd_dirty _spd_base _spd_identical _spd_path _spd_note
    fi
    unset _spd_dirty
fi

# ---- the closing rule: an incident resolved without a runbook is not resolved ----------
# An incident that leaves no runbook behind is a POISONABLE condition, not a number on a
# pane. The Ops brief has called this rule "not optional" since it was written, and it was
# disobeyed six times in one day — which is the whole difference between an instruction and
# a fence.
#
# THE THREE HONEST ENDINGS, and each is one command (see the persona's own fayth, which is
# where the rule is declared and where they are enumerated):
#
#   nothing on the shelf fit; I diagnosed something new   ->  sop.sh write
#   an SOP fit but was incomplete                         ->  sop.sh write   (the upsert)
#   an SOP fit and its CHECK confirmed                    ->  sop.sh applied --check pass
#
# THE THIRD ROW IS WHAT KEEPS THIS FROM FIRING ON A GOOD SESSION. "The SOP fit, it held, and
# it taught us nothing new" is the outcome a healthy shelf produces most of the time and it
# is creditable; poisoning that session would punish the good case. SILENCE is what is being
# outlawed here, not brevity — and recording the truth is the cheapest of the three ways out
# whatever the truth turns out to be, which is the property that keeps this from becoming a
# gate an aeon satisfies hollowly.
#
# `--held` DOES NOT ENTER INTO IT, deliberately. `--held no` and `--held unknown` are honest
# outcomes of a runbook that genuinely fit, and demanding an amendment on top of one would
# make `--held yes` the cheapest exit — a lie, and one that corrupts the single field the
# whole shelf is measured by. What does not satisfy the rule is `--check fail` alone: that is
# the session's own statement that nothing on the shelf applied, which is row one, and row
# one's exit is a write.
#
# ONLY FOR A PERSONA THAT DECLARES THE RULE. A builder closing a bead without touching an
# SOP is doing exactly its job.
#
# IT DECLINES TO JUDGE WHEN IT CANNOT READ. An unreadable shelf or an unreadable ledger is
# not an absence, and treating one as an absence would poison every incident closed on the
# day the database is down — the day a runbook is worth most (law-absence-needs-a-positive-control).
SOP_SILENT=""
if [ "$SOP_REQUIRED" = 1 ] && [ "$st" = "closed" ] && [ "$superseded" != 1 ]; then
    shelf_after=""; shelf_after_ok=0
    if shelf_after="$("$SPIRA_HOME/sop.sh" digest 2>/dev/null)"; then shelf_after_ok=1; fi

    # 0 recorded, 1 read and no such record, 2 unreadable. Anything else is sop.sh itself
    # failing to run, which is the same answer as unreadable: not an absence.
    sop_applied=0
    "$SPIRA_HOME/sop.sh" log --bead "$BEAD_ID" --check pass --since "$SESSION_EPOCH" \
        >/dev/null 2>&1 || sop_applied=$?
    read -r sop_wrote sop_verdict <<< "$(sop_rule_verdict "$SHELF_BEFORE_OK" "$SHELF_BEFORE" "$shelf_after_ok" "$shelf_after" "$sop_applied")"
    printf '%s spira: %s: %s closing-rule wrote=%s applied=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$FAYTH" "$BEAD_ID" "$sop_wrote" "$sop_applied"
    if [ "$sop_verdict" = satisfied ]; then
        :
    elif [ "$sop_verdict" = decline ]; then
        printf '%s spira: %s: %s closing rule NOT judged — the shelf or the applications ledger could not be read (wrote=%s applied=%s). Absence is not proven, so nothing is poisoned.\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$FAYTH" "$BEAD_ID" "$sop_wrote" "$sop_applied"
    else
        # The close is undone AND the bead is taken out of circulation, because this is not
        # a bead the next aeon should retry blind: a session already resolved the incident
        # and kept what it learned to itself, and the recovery is a human deciding what the
        # runbook should have said. POISONED is in the log line on purpose — it is one of
        # the strings the operator's panes treat as actionable, so this reaches somebody
        # without a second notification path to build and forget.
        bead_reopen "$BEAD_ID" no-sop "Reopened and poisoned by aeon.sh: this incident was closed and no runbook came out of it. The session recorded neither an SOP written or amended (sop.sh write) nor a runbook whose CHECK confirmed (sop.sh applied --check pass), so nothing on the shelf is any better for this incident having happened and the next occurrence costs exactly as much. The closing rule is not optional: an incident resolved without an SOP must produce one. To clear this, write the runbook this incident should have left — or, if one already fitted and held, record it — then remove the spira-poison label."
        bdq label add "$BEAD_ID" spira-poison >/dev/null 2>&1
        printf '%s spira: %s: %s REOPENED and POISONED — closed with no runbook written and no SOP application recorded\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$FAYTH" "$BEAD_ID"
        SOP_SILENT=1
        # THE ATTEMPT COUNTER MUST NOT ALSO CHARGE FOR THIS. The bead is open because this
        # process reopened it, and the teardown cannot see that: it reads the session's trace,
        # which is of a session that committed, closed and ran to its own end. Left to itself
        # it writes a note about a worker that "did not survive to judge this bead" onto a
        # bead whose session finished perfectly well, and adds a rung for it. The poison IS
        # the verdict here and it needs no second counter behind it
        # (law-charge-only-a-named-outcome).
        #
        # ONLY WHEN THE SESSION COMMITTED. A session that closed with nothing committed was
        # already reopened above for that, and THAT is a verdict about the work which the
        # attempt counter should keep charging — the requeue text would say the session did
        # the work, which it did not.
        if [ "$committed" = "yes" ]; then
            REQUEUE_CAUSE="sop-silent"
            REQUEUE_WHY="The incident was closed with no runbook behind it, so the close was undone and the bead poisoned; that poison is the verdict and this counter is not."
        fi
    fi
fi

# ---- close-reason fence: refuse a reason that names its own remainder ------------------
# A close reason containing a statute phrase (law-no-close-reason-admits-unfinished:
# "PERMANENT FIX NEEDED", "temporary workaround", etc.) says the work is not done.
# The remainder is a bead, never a sentence in the close reason.
#
# THE FENCE AND detect_invalid_closed IN lib.sh SHARE close-reason-flags.py so they
# cannot disagree about which phrase triggers a refusal.
#
# Two honest endings when this fires:
#   (a) file the remainder with bead.sh, cite its id in the reason, then close
#   (b) leave the bead open: groomer.sh depends-on-fix <bead> --fix <blocker-bead>
#
# Override: SPIRA_CLOSE_REASON_OVERRIDE=<why this phrase is not a remainder>
if [ "$st" = "closed" ] && [ "$committed" = "yes" ] && [ -z "$SOP_SILENT" ]; then
    _cr_raw="$(bdjson show "$BEAD_ID" 2>/dev/null | python3 -c '
import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
d=d if isinstance(d,list) else [d]
if d: print((d[0].get("close_reason") or ""))' 2>/dev/null)" || _cr_raw=""
    if [ -n "$_cr_raw" ] && [ -z "${SPIRA_CLOSE_REASON_OVERRIDE:-}" ]; then
        _cr_hit="$(python3 "$SPIRA_HOME/close-reason-flags.py" "$_cr_raw" 2>/dev/null)" || _cr_hit=""
        if [ -n "$_cr_hit" ]; then
            bead_reopen "$BEAD_ID" unfinished-reason "Reopened by aeon.sh: close reason contains a statute phrase (\"$_cr_hit\") that says the work is not done (law-no-close-reason-admits-unfinished). A remainder is a bead, not a sentence in the close reason. Two endings: (a) file the remainder with bead.sh, cite its id in the reason, then close; (b) groomer.sh depends-on-fix $BEAD_ID --fix <blocker-bead> if a fix is already in flight (law-a-bug-with-a-fix-in-flight-depends-on-it)."
            printf '%s spira: %s: %s REOPENED — close reason contains statute phrase: %s. Override: SPIRA_CLOSE_REASON_OVERRIDE=<why>\n' \
                "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$FAYTH" "$BEAD_ID" "$_cr_hit"
            st="open"
            REQUEUE_CAUSE="unfinished-reason"
            REQUEUE_WHY="Close reason contained a statute phrase (\"$_cr_hit\"). File the remainder as a bead, cite its id in the reason, then re-close."
        fi
    fi
    unset _cr_raw _cr_hit
fi

# ---- the groom escalation rule: a claimed ESCALATED must be backed by an ask bead -----
# A groom pass that writes ESCALATED, inquiry, or flagged for bead X in its log without a
# matching ask bead created in the same session has not escalated — it has claimed to.
# The sentinel only verifies the log line exists (law-a-matcher-reads-code-not-prose);
# this check reads what the session actually wrote in the database.
#
# WINDOW: bead IDs extracted from log lines written by THIS session (lines after
# GROOM_LOG_LINES_BEFORE). Ask beads are type=decision, SPIRA_ASK_LABEL label, created
# at or after SESSION_EPOCH, with the bead ID in their title.
#
# DECLINES TO JUDGE on unreadable log or db — absence is not proven in that case.
GROOM_SILENT=""
if [ "$GROOM_ESCALATION_CHECK" = 1 ] && [ "$st" = "closed" ] && [ "$superseded" != 1 ] && [ -z "$SOP_SILENT" ]; then
    _groom_log_path="${SPIRA_RUN}/groom.log"
    _groom_new=""
    if [ -f "$_groom_log_path" ]; then
        _groom_new="$(tail -n +"$((GROOM_LOG_LINES_BEFORE + 1))" "$_groom_log_path" 2>/dev/null)"
    fi
    if [ -n "$_groom_new" ] && grep -qiE 'ESCALATED|inquiry|flagged' <<< "$_groom_new"; then
        _ask_json="$(bdq list --type decision \
            --label "$SPIRA_ASK_LABEL" --json 2>/dev/null)" || _ask_json=""
        _unproven="$(groom_claims_verified "$_groom_new" "${_ask_json:-[]}" "$SESSION_EPOCH")"
        if [ -n "$_unproven" ]; then
            bead_reopen "$BEAD_ID" no-groom-ask \
                "Reopened and poisoned: groom log claimed ESCALATED for $_unproven but no ask bead was filed in this session naming those beads. A log claim is not an escalation. File the ask via mail.sh send operator --kind question, then re-run the pass."
            bdq label add "$BEAD_ID" spira-poison >/dev/null 2>&1
            printf '%s spira: %s: %s REOPENED and POISONED — groom log claimed ESCALATED for %s but no ask bead found in this session\n' \
                "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$FAYTH" "$BEAD_ID" "$_unproven"
            GROOM_SILENT=1
            if [ "$committed" = "yes" ]; then
                REQUEUE_CAUSE="groom-silent"
                REQUEUE_WHY="Groom log claimed escalation for $_unproven without a matching ask bead; the close was undone and the trigger poisoned."
            fi
        else
            printf '%s spira: %s: %s groom-escalation-check: all claimed escalations verified\n' \
                "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$FAYTH" "$BEAD_ID"
        fi
    fi
fi

# CLOSED BEHIND THE BASE IS NOT FINISHED. The brief asked for a rebase as the last step; this
# is the check that it happened, and the fallback when it did not. The session is over, the
# claim is still this process's, so rewriting the branch here rewrites nothing beneath
# anyone. Three outcomes, each named in the log so the brief's effect can be measured:
#   current  — the session rebased (or nothing landed meanwhile); nothing to do
#   rebased  — it did not, but the replay was clean; the harness did it and says so
#   reopened — it did not, and the replay conflicts; the next aeon is handed the rebase
#              with the paths named, exactly as the landing pass would have, only sooner
#
# NOT AFTER A CLOSE THAT WAS UNDONE. The closing-rule check above reopens and poisons; there
# is no longer a close whose currency is worth judging, and a "rebased after the session
# closed the bead" note on a bead this same process just reopened contradicts itself in the
# one place a reader looks for what happened.
if [ "$st" = "closed" ] && [ "$committed" = "yes" ] && [ -z "$SOP_SILENT" ] && [ -z "$GROOM_SILENT" ]; then
    if [ -n "$BASE_REMOTE" ]; then
        git -C "$REPO" fetch -q "$BASE_REMOTE" 2>/dev/null \
            || log "$FAYTH: fetch of $BASE_REMOTE failed — judging currency against a possibly stale $BASE"
    fi
    if git -C "$REPO" merge-base --is-ancestor "$BASE_FQREF" "refs/heads/$BRANCH" 2>/dev/null; then
        log "$FAYTH: $BEAD_ID closed current with $BASE"
    elif rebase_branch "$BRANCH" "$BASE_FQREF" "$REPO" "$REPO_NAME"; then
        log "$FAYTH: $BEAD_ID closed behind $BASE — rebased by the harness after close (the session did not)"
        bdq note "$BEAD_ID" "Rebased onto $BASE by aeon.sh after the session closed the bead without doing so. The replay was clean; the landing gate judges the rebased tree." >/dev/null 2>&1 || true
    else
        _cited_sha="$(bead_cited_commit_on_base "$BEAD_ID" "$REPO" "$BASE_FQREF" 2>/dev/null)" || _cited_sha=""
        if [ -n "$_cited_sha" ]; then
            log "$FAYTH: $BEAD_ID closed behind $BASE but notes cite $_cited_sha on $BASE — retiring as landed"
            land_mark "$BEAD_ID" LANDED "$_cited_sha" "cited-on-main"
            spira_destroy_branch "$BEAD_ID" "$BRANCH" "$REPO" \
                "fix on $BASE cited in notes as $_cited_sha" "cited-landed" \
                || log "$FAYTH: $BEAD_ID branch retire failed"
        else
            _other_beads="$(other_beads_on_conflicts "$REPO" "$BRANCH" "$BASE" "${REBASE_CONFLICTS:-}")"
            _reopen_note="Reopened by aeon.sh: closed behind $BASE and $BRANCH does not rebase onto it — conflicts in ${REBASE_CONFLICTS:-unknown}. The brief asked for this rebase before closing."
            if [ -n "$_other_beads" ]; then
                _reopen_note="$_reopen_note Those files were changed on $BASE by $_other_beads — check whether this work is already landed before resolving."
            else
                _reopen_note="$_reopen_note A merge conflict is not an escalation — the next aeon is handed the rebase and must resolve it."
            fi
            bead_reopen "$BEAD_ID" rebase-conflict "$_reopen_note"
            # THE TEARDOWN MUST NOT READ THIS BACK AS A FAILURE OF THE WORK. The work is committed
            # and the session closed on it; what is missing is a rebase over commits that landed
            # while it ran, which is a fact about the queue. Charging it made the busiest branches
            # the likeliest to poison.
            REQUEUE_CAUSE="rebase-conflict"
            REQUEUE_WHY="$BRANCH would not rebase onto $BASE (conflicts in ${REBASE_CONFLICTS:-unknown}); the next aeon is handed the rebase."
            log "$FAYTH: $BEAD_ID REOPENED — closed behind $BASE, conflicts in ${REBASE_CONFLICTS:-unknown}"
        fi
    fi
fi
