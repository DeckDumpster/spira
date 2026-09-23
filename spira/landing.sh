#!/usr/bin/env bash
# landing.sh — CHECK 6, the landing pass over every repository.
#
# RESPONSIBILITY MAP
# land_repo (below): per-branch pipeline per mode
#
#   step                             push   pr    hold  queue
#   ──────────────────────────────── ─────  ────  ────  ─────
#   rebase_branch onto base           ✓      ✓     ✓     ✓
#   confine.sh check                  ✓      ✓     ✓
#   gate.sh                           ✓      ✓     ✓
#   land_mark CERTIFIED                                   ✓
#   merge + push to base              ✓
#   land_pr (open pull request)              ✓
#   note bead, hold for hand                       ✓
#   rebase_survivors after landing    ✓
#
# land-modes: push pr hold queue
#
# After land_repo loops (this file):
#   queue.sh step (verdict.sh + batch.sh) — queue mode only
#   skew.sh refresh (advance checkout)    — push and queue
#
# queue.sh step (called from here, queue mode only):
#   verdict.sh: read CI status; fast-forward or requeue or attribute red
#   batch.sh:   open next batch when count or age threshold is met
#
# sentinel CHECK6 hand-off: launched as transient unit `spira-landing`.
#   Unit name is the mutex; --collect ensures a FAILED unit does not block later runs.
#   Writes landing.status (key=value, rewritten each run) and landing.progress
#   (append-only, drained by the sentinel) back across the seam.
#
# skew.sh refresh (push, queue): advances home checkout to current base.
#
# deploy.sh (operator-run, mode-independent):
#   Drain, swap SPIRA_PROD symlink, restart units, health-check, rollback on failure.
#
# Certifying (queue mode) requires bead status = closed.
# Poisoned/unclaimable beads have status != closed and are already excluded.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

STATUS="$SPIRA_RUN/landing.status"
MAILBOX="$SPIRA_RUN/landing.progress"
LAND_RUN="$SPIRA_RUN/landing.run"
LAND_CONTAINERS="$SPIRA_RUN/landing.containers"

# n_branches is the positive control's evidence and it counts every spira/* ref seen, not
# the subset that was landable. "I looked at nine branches and moved none" is a claim about
# the graph; "I looked at zero" is a claim about this program, and only the raw count can
# tell them apart.
n_branches=0
n_prog=0

# THE SWEEP'S OWN METER, and it is a pair on purpose. A post-landing rebase is worth its
# seconds only if branches actually come through it clean; one that only ever conflicts has
# moved a reopen earlier and saved nobody a session. Both halves are printed on the
# pass-complete line, so the question is settled from landing.log with no new machinery
# (law-take-the-simple-fix-with-a-meter).
n_swept=0
n_swept_conflict=0

# The mailbox line is the message and nothing else — no ACT prefix, no timestamp. The
# sentinel adds both when it counts it, and a line that arrived pre-formatted would read as
# though the sentinel had done the work.
#
# ONLY MOVEMENTS CROSS THE SEAM. `act` writes to this worker's log and stops there: the
# sentinel's `acted` counter exists for its own pass summary and gates nothing, while
# `progressed` gates the judgement tier, so a write that moved nothing has no business
# travelling. Sending a "gated and held" across would be the old blinding bug — a futile
# action muting the only check that notices paralysis — rebuilt across a file.
act()      { log "$*"; }
progress() { n_prog=$((n_prog+1)); log "$*"; printf '%s\n' "$*" >> "$MAILBOX"; }

# STATUS IS WRITTEN ON EVERY EXIT PATH, including the one where systemd's RuntimeMaxSec cuts
# this off mid-gate. A worker that only reports when it finishes cleanly is a worker whose
# silence means nothing.
finish() {
    local rc=$?
    { printf 'SP_LAND_AT=%s\n'       "$(date +%s)"
      printf 'SP_LAND_RC=%s\n'       "$rc"
      printf 'SP_LAND_BRANCHES=%s\n' "$n_branches"
      printf 'SP_LAND_MOVED=%s\n'    "$n_prog"
    } > "$STATUS.$$" 2>/dev/null && mv -f "$STATUS.$$" "$STATUS" 2>/dev/null
    rm -f "$STATUS.$$" 2>/dev/null
    rm -f "$LAND_RUN" "$LAND_CONTAINERS" 2>/dev/null
    exit "$rc"
}
# EXECUTABLE FROM HERE. Everything below runs a landing pass — the EXIT trap that writes the
# status file, the pass clock, the log line, and the loop over every repository. Sourcing this
# file to borrow one function (content_landed is the one worth borrowing) otherwise performs a
# live pass over every branch as a side effect of the `.`, which is how a read-only diagnostic
# became a real landing twice in one afternoon. The first guard here covered only the loop, so
# the traps and the "starting a pass" line still fired and the file still LOOKED like it ran.
# A partial guard on a side effect is worse than none: it makes the remaining half harder to
# see. Guarded the way harness.sh guards itself.
if [ "${BASH_SOURCE[0]}" != "$0" ]; then return 0 2>/dev/null || true; fi

# ======================================================================================
# HALT SUBCOMMAND — stop a running pass cleanly.
#
#   landing.sh halt [--reason <text>] [--dry-run]
#
# Signals the pass and waits for it to finish its current step (bounded by
# SPIRA_HALT_GRACE, default 30s) before escalating to SIGKILL. Tears down
# containers by name from the registry (LAND_CONTAINERS), removes unpushed
# batch branches, and writes an interrupt record so the reason is durable.
#
# --dry-run: reports whether a pass is running and what would be cleaned up,
# touching nothing. Exits non-zero when no pass is running.
# ======================================================================================
_land_pid_alive() {   # _land_pid_alive <pid> -> 0 when pid exists and is not ourselves
    [ -n "$1" ] || return 1
    [ "$1" != "$$" ] || return 1
    [ -d "/proc/$1" ]
}

_halt_cleanup_orphan_branches() {
    local name repo open_br rbr open_file
    for name in $(spira_repos 2>/dev/null); do
        [ "$(repo_land "$name" 2>/dev/null)" = "queue" ] || continue
        repo="$(repo_root "$name" 2>/dev/null)" || continue
        open_br=""
        open_file="${SPIRA_QUEUE_DIR:-$SPIRA_RUN/queue}/$name/open"
        if [ -r "$open_file" ]; then
            while IFS='=' read -r k v; do
                [ "$k" = branch ] && { open_br="$v"; break; }
            done < "$open_file"
        fi
        while IFS= read -r rbr; do
            [ -n "$rbr" ] || continue
            [ "$rbr" = "$open_br" ] && continue
            # Pushed branches have a remote tracking ref — leave them alone.
            if git -C "$repo" for-each-ref "refs/remotes/*/$rbr" 2>/dev/null | grep -q .; then
                continue
            fi
            printf 'landing halt: removing orphaned batch branch %s in %s\n' "$rbr" "$name"
            SPIRA_REF_SANCTIONED=1 git -C "$repo" branch -D "$rbr" 2>/dev/null \
                || printf 'landing halt: WARNING — could not remove %s\n' "$rbr"
        done < <(git -C "$repo" for-each-ref --format='%(refname:short)' \
                     'refs/heads/spira/queue/*' 2>/dev/null)
    done
}

_cmd_halt() {
    local reason="" dry_run=0 grace="${SPIRA_HALT_GRACE:-30}"
    while [ $# -gt 0 ]; do
        case "$1" in
        --reason)     [ $# -ge 2 ] || { printf 'landing halt: --reason requires an argument\n' >&2; exit 2; }
                      reason="$2"; shift 2 ;;
        --reason=*)   reason="${1#--reason=}"; shift ;;
        --dry-run)    dry_run=1; shift ;;
        --*)  printf 'landing halt: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)    printf 'landing halt: unexpected argument: %s\n' "$1" >&2; exit 2 ;;
        esac
    done

    local pid="" started="" repo="" branch="" phase=""
    if [ -r "$LAND_RUN" ]; then
        while IFS='=' read -r k v; do
            case "$k" in
            pid)     pid="$v" ;;
            started) started="$v" ;;
            repo)    repo="$v" ;;
            branch)  branch="$v" ;;
            phase)   phase="$v" ;;
            esac
        done < "$LAND_RUN"
    fi

    # Read container registry before signaling — a clean pass exit deletes the file.
    local containers=()
    if [ -r "$LAND_CONTAINERS" ]; then
        while IFS= read -r cname; do
            [ -n "$cname" ] && containers+=("$cname")
        done < "$LAND_CONTAINERS"
    fi

    local running=0
    _land_pid_alive "${pid:-}" && running=1

    if [ "$dry_run" = 1 ]; then
        if [ "$running" = 0 ]; then
            printf 'landing: no pass running\n'
            exit 1
        fi
        local elapsed="-"
        [ -n "${started:-}" ] && elapsed="$(( $(date +%s) - started ))s"
        printf 'landing: pass running — pid=%s elapsed=%s repo=%s branch=%s phase=%s\n' \
            "$pid" "$elapsed" "${repo:--}" "${branch:--}" "${phase:--}"
        local cname
        for cname in "${containers[@]:-}"; do
            [ -n "$cname" ] && printf 'landing: would tear down container %s\n' "$cname"
        done
        exit 0
    fi

    if [ "$running" = 0 ]; then
        printf 'landing: no pass running — nothing to halt\n' >&2
        exit 1
    fi

    local elapsed="-"
    [ -n "${started:-}" ] && elapsed="$(( $(date +%s) - started ))s"

    # Write interrupt record before signaling so the reason is durable.
    local int_file="$SPIRA_RUN/landing.interrupted"
    { printf 'halted=%s\n'   "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      printf 'reason=%s\n'   "${reason:-unstated}"
      printf 'pid=%s\n'      "${pid:--}"
      printf 'elapsed=%s\n'  "$elapsed"
      printf 'repo=%s\n'     "${repo:--}"
      printf 'branch=%s\n'   "${branch:--}"
      printf 'phase=%s\n'    "${phase:--}"
    } > "$int_file.$$" 2>/dev/null && mv -f "$int_file.$$" "$int_file" 2>/dev/null

    printf 'landing: halting pass pid=%s elapsed=%s repo=%s branch=%s phase=%s reason=%s\n' \
        "$pid" "$elapsed" "${repo:--}" "${branch:--}" "${phase:--}" "${reason:-unstated}"

    kill -TERM "$pid" 2>/dev/null || true

    local waited=0
    while [ "$waited" -lt "$grace" ]; do
        [ -d "/proc/$pid" ] || break
        sleep 1
        waited=$(( waited + 1 ))
    done

    if [ -d "/proc/$pid" ]; then
        printf 'landing: pass did not stop after %ss — sending SIGKILL\n' "$grace"
        kill -KILL "$pid" 2>/dev/null || true
        sleep 1
    fi

    # After SIGKILL the EXIT trap does not run — clean up state files here.
    rm -f "$LAND_RUN" "$LAND_CONTAINERS" 2>/dev/null || true

    # Tear down containers by name from the record.
    local cname
    for cname in "${containers[@]:-}"; do
        [ -n "$cname" ] || continue
        if podman ps --format '{{.Names}}' 2>/dev/null | grep -qxF "$cname"; then
            printf 'landing: tearing down container %s\n' "$cname"
            bash "${SPIRA_PROD:-$SPIRA_HOME}/testenv.sh" down --name "$cname" >/dev/null 2>&1 \
                || printf 'landing: WARNING — could not tear down container %s\n' "$cname"
        fi
    done

    _halt_cleanup_orphan_branches

    printf 'landing: halted — interrupted at phase=%s in %s (%s)\n' \
        "${phase:--}" "${repo:--}" "$elapsed"
}

case "${1:-}" in
halt) shift; _cmd_halt "$@" ;;
esac

trap finish EXIT
trap 'exit 143' TERM INT

# ======================================================================================
# NEVER START A GATE THIS PASS CANNOT FINISH.
#
# systemd cuts this worker off at RuntimeMaxSec. A pass killed mid-gate keeps every branch it
# already pushed — a push is durable and `finish` records the count on the TERM path — so the
# cap never lost work. What it did was worse in a quieter way: the pass restarts from the top
# next time, re-gates the SAME branch from scratch, and is killed at the same point. Measured
# 2026-09-07, four consecutive passes exited 143 having moved nothing, while two closed beads
# sat unlanded and the base ref went six hours without a commit. Zero progress, forever, with
# every pass looking merely slow.
#
# The cap alone cannot fix that: raising it moves the cliff to wherever the next slow gate is.
# What removes the loop is refusing to BEGIN a gate there is not time to finish, so a pass
# always ends cleanly, always keeps what it landed, and always hands the rest to its successor.
#
# The reserve is deliberately generous. Under-reserving costs a whole pass; over-reserving
# costs one branch's turn, and the next pass is two minutes away.
PASS_START="$(date +%s)"
LAND_MAXSEC="${SPIRA_LAND_MAXSEC:-3600}"     # what the dispatcher gave us, or the same default

# _land_state — write pass state atomically so halt can report what was interrupted.
# Called at key phase transitions in land_repo(); always includes pid and started.
_land_state() {   # _land_state [key=value...]
    { printf 'pid=%s\nstarted=%s\n' "$$" "$PASS_START"
      printf '%s\n' "$@"
    } > "$LAND_RUN.$$" 2>/dev/null \
    && mv -f "$LAND_RUN.$$" "$LAND_RUN" 2>/dev/null || true
}
_land_state
export SPIRA_LANDING_CONTAINERS="$LAND_CONTAINERS"
: > "$LAND_CONTAINERS" 2>/dev/null || true
LAND_GATE_RESERVE="${SPIRA_LAND_GATE_RESERVE:-1200}"

# gate_fits -> 0 if there is room for another gate in this pass, 1 if the pass should stop.
# ZERO OR NEGATIVE MEANS NO LIMIT, which is how a hand-run pass (no RuntimeMaxSec at all)
# behaves: an operator draining a backlog must not be told there is no time left by a budget
# that is not being enforced on them.
gate_fits() {
    [ "${LAND_MAXSEC:-0}" -gt 0 ] 2>/dev/null || return 0
    local spent=$(( $(date +%s) - PASS_START ))
    [ $(( LAND_MAXSEC - spent )) -ge "$LAND_GATE_RESERVE" ]
}

# gate_lock_wait — sets _gate_wait to how long this pass may wait for the gate tree.
#
# DERIVED FROM THE GATE TIMEOUT, not the pass budget. gate.sh documents why the wait must
# be at least 2 * SPIRA_GATE_TIMEOUT: one holder can legitimately run two full trials
# (branch + base), so a shorter wait times out against a healthy holder. An explicit
# SPIRA_GATE_LOCK_WAIT is honored so the operator can size the two independently. When the
# remaining pass budget is shorter than the ideal, the wait is capped and logged so rc=75
# is readable as contention rather than as a branch fault.
#
# Sets _gate_wait (not stdout) so log() messages are not consumed by a $() caller.
gate_lock_wait() {
    if [ -n "${SPIRA_GATE_LOCK_WAIT:-}" ]; then
        _gate_wait="$SPIRA_GATE_LOCK_WAIT"
        return
    fi
    local ideal=$(( ${SPIRA_GATE_TIMEOUT:-2700} * 2 ))
    if [ "${LAND_MAXSEC:-0}" -gt 0 ]; then
        local remaining=$(( LAND_MAXSEC - ($(date +%s) - PASS_START) ))
        if [ "$remaining" -lt "$ideal" ]; then
            log "landing: gate lock wait capped at ${remaining}s by pass budget (ideal ${ideal}s); rc=75 should be read as contention"
            _gate_wait="$remaining"
            return
        fi
    fi
    _gate_wait="$ideal"
}

# HOW MANY CERTIFICATION GATES RUN IN PARALLEL THIS PASS. Derived from the box when
# SPIRA_CERTIFY_PAR is unset: min(nproc --all/4, free-memory/400MiB), at least 1.
# nproc --all reads /proc/cpuinfo directly, not cgroup limits (law-measure-inside-the-fence).
certify_pass_par="${SPIRA_CERTIFY_PAR:-}"
_certify_par_detail=""
if [ -z "$certify_pass_par" ]; then
    _cp_np=$(nproc --all 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo 4)
    _cp_mem=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo 2>/dev/null || echo 1600)
    _cp_by_cpu=$(( _cp_np / 4 ))
    _cp_by_mem=$(( _cp_mem / 400 ))
    certify_pass_par=$(( _cp_by_cpu < _cp_by_mem ? _cp_by_cpu : _cp_by_mem ))
    _certify_par_detail=" (cpu ${_cp_np}/4, mem ${_cp_mem}/400)"
    unset _cp_np _cp_mem _cp_by_cpu _cp_by_mem
fi
[ "${certify_pass_par:-0}" -lt 1 ] && certify_pass_par=1

log "landing: starting a pass over [$(spira_repos | tr '\n' ' ')] certify_par=${certify_pass_par}${_certify_par_detail}"
unset _certify_par_detail

# THE VERDICT CACHE IS PRUNED HERE, once a pass, because this is the only thing that runs on
# a clock and already touches every repository. Entries are keyed by content — an entry can
# only be hit by the identical tree, file list, command and harness — so what is left behind
# is clutter, and clutter that grows by one file per gated tree forever.
#
# IT PRUNES AT THE SAME AGE THE GATE REFUSES TO READ AT, and that is the whole reason this
# line takes the key rather than a number of its own. The gate expires an entry on its `at=`
# stamp; this deletes it from the disk. Two numbers here would be two answers to "how long
# does a verdict live", and the operator would have tuned one of them.
#
# ONE FILE AT A TIME and never the directory: deleting the directory would race a gate
# writing into it, and the entries are individually disposable.
verdict_ttl="${SPIRA_VERDICT_TTL:-0}"
case "$verdict_ttl" in ''|*[!0-9]*) verdict_ttl=0 ;; esac
find "${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}" -maxdepth 1 -type f \
    -mmin "+$(( verdict_ttl / 60 ))" -delete 2>/dev/null || true

# ======================================================================================
# LAND FINISHED BRANCHES. A passing branch must merge without a human; a branch
# that waits rots, because main moves underneath it and manufactures conflicts that did
# not exist.
#
# ONCE PER REPOSITORY, AND HOW A BRANCH LANDS IS THE REPOSITORY'S OWN ANSWER. brain has no
# reviewer between an aeon's commit and origin/main, so its branches are merged and pushed.
# A repository may have branch protection and a CI suite that is the real authority, so pushing
# its default branch would be both wrong and refused; its branches become pull requests with
# auto-merge armed, and GitHub lands them when CI goes green (law-green-prs-merge-themselves).
# A repository whose branches this harness has no business advancing — one whose checkout is a
# Town rig on a detached HEAD — has its branches gated and held.
#
# AND THE BRANCH THEY LAND ON IS THE REPOSITORY'S OWN ANSWER TOO. Three of the seven are not
# `main`: some repositories default to `master` and have no ref named `main` at all, and a
# remote need not be called `origin`. `spira_landref` resolves it and refuses to guess.
# ======================================================================================
# Land in a DEDICATED WORKTREE, never in the shared checkout an interactive session is
# using. The first version guarded on `git status --porcelain` being clean, which fails
# safe in the wrong direction: .claude/concierge.log was a LOG TRACKED IN GIT, rewritten
# every ten minutes by concierge.timer, so the tree was dirty essentially always and the
# gate never opened once. A gate that never opens is as broken as one that never closes.
# The logs are untracked now, but the structural answer is not to care about that tree.
#
# EVERY BRANCH IS REBASED ONTO ITS REPOSITORY'S BASE BEFORE IT IS GATED OR MERGED. A branch
# that was cut hours ago is measured against a base that has moved, and because every Spira bead
# edits the same few files under .claude/spira, the collision is manufactured rather than
# real. Rebasing first turns the merge into a fast-forward and puts any genuine conflict in
# front of an aeon, which can resolve it, instead of in front of a `git merge` that can only
# fail. A merge conflict is not an escalation.

# submitted <id> <tip> [state] — has this exact branch tip already been sent, and how?
#
# `pr` and `hold` leave the branch standing by design, so the plain ancestry test that stops
# `push` mode from re-landing says nothing about them: without this, every pass two minutes
# apart would push the branch again, ask GitHub for the pull request again, and re-note the
# bead again, forever. Keyed on the TIP, so a branch an aeon has moved is genuinely resent.
# A failed submission is retried, but not sooner than an hour — a gh outage must not become
# a request every two minutes, and it must not become silence either.
# =======================================================================================
# D2 — THE PASS REMEMBERS WHAT IT DID.
#
# This pass ran every two minutes and re-derived the entire world each time, keeping nothing.
# So it contradicted its own work: it landed sp-n21 at 21:51:17, reaped the branch a minute
# later, and at 22:00:55 wrote "reopened sp-n21 — does not rebase onto origin/main" about the
# work it had merged nine minutes earlier (sp-q9i). Nine more beads were reopened in one day
# for rebases that were mostly clean, each costing a full Opus session (sp-118). Every one of
# those is a pass with no memory reaching a conclusion its predecessor had already refuted.
#
# ONE FILE PER BEAD, holding the last transition and the commit it was about:
#
#   DONE -> GATED -> REBASED -> LANDED -> SENT
#                 \-> RED      the branch's own failure; reopened ONCE
#                 \-> BLOCKED  needs the operator; never silently retried
#
# THE TIP IS PART OF THE STATE, not just the name. A bead whose branch an aeon has moved is
# genuinely new work and must be re-judged; a bead whose branch has not moved since it was
# landed is the case above, and the state is what says so. Without the tip this would be a
# memory that goes stale silently, which is worse than none.
#
# IT IS AUTHORITATIVE FOR "ALREADY LANDED" AND ADVISORY FOR EVERYTHING ELSE. A recorded
# LANDED forbids a reopen outright, because the alternative — putting merged work back on the
# board — costs an aeon and can revert an amendment. Every other state only lets the pass
# skip work it has already done; if the file is missing, deleted or unreadable, the pass
# behaves exactly as it did before, which is why $SPIRA_RUN can still be wiped at any time.
# =======================================================================================
# THE RECORD IS USED IN TWO WAYS. The first guard written — "do not reopen a commit this
# pass already landed" — was written, then proved UNREACHABLE: a landed tip that has not
# moved is an ancestor of the base, so `content_landed` returns true and the pass never
# reaches the rebase or the gate at all; and a tip that HAS moved is new work, which the
# record correctly declines to vouch for. There is no state in between.
#
# So sp-q9i is NOT fixed here, and shipping that guard would have looked exactly like fixing
# it. Something recreated that ref between the reap and the next pass, and until what did is
# established from the logs rather than guessed at, a guard against it is a guess with a
# comment attached. sp-q9i keeps that question.
#
# THE SECOND USE is the assertion in sending.sh (sp-bjzj). Before it reaps a branch,
# sending.sh checks that landing.sh left a landstate entry for it. An absent entry means
# landing.sh never selected this branch — the selection bug that caused sp-qj8n to close
# four times without the work ever reaching the base. Every code path here writes a record:
# GATED / REBASED / RED / CONTENT / LANDED, so any branch that slips past the loop is
# visible on the first reap rather than after four reopen cycles.
#
# What the record IS for originally: the stretch between DONE and LANDED is invisible, and
# every fact needed to show it is already computed and then dropped (sp-idml). One line per
# bead, written where the transition happens, costs nothing and is the input any analysis
# of that stretch will need.

# =======================================================================================
# A BASE THAT FAILS ITS OWN GATE IS THE REPOSITORY'S BUG, AND IT NEEDS AN OWNER
# =======================================================================================
# BASE_FAIL already costs the branch nothing — no reopen, no attempt. That is the half that
# stops the harm; on its own it also means nothing is ever done about it. A repository whose
# gate is red against its own base refuses EVERY branch of that repository, and the only
# trace was a log line saying the next pass would take it, repeated every two minutes. The
# reopens are gone; the silence that replaced them is the other half of the same defect.
#
# SO THE FINDING GETS AN IDENTITY: one bead, in the builder's partition, labelled with the
# repository whose base is broken. Filed through incident.sh because that intake already
# spools the payload before touching the database, dedupes on an external ref, bumps a
# recurrence instead of filing a second, and escalates once past SIN_AT recurrences — none of
# which is worth a second implementation, and the recurrence count is exactly the signal
# wanted here: a base red for five passes is a base nobody is fixing.
#
# THE REF IS THE REPOSITORY AND THE SUITE, and deliberately not the branch or the bead. Five
# branches blocked by one broken base are one incident, not five; that is the whole meaning
# of "idempotent" here, and a ref carrying either identifier would file one bead per branch
# per pass and rebuild the 300-copy queue the dedupe exists to prevent. The suite comes from
# the gate's own VERDICT line rather than from its prose, so a reworded message cannot
# silently split one incident into two.
#
# IT IS A DEFECT, NOT AN OUTAGE, so the labels are the builder's rather than Ops's: fixing a
# red suite on a base means changing code, and Ops has eight minutes and a runbook.
INC="${SPIRA_INCIDENT:-$SPIRA_HOME/incident.sh}"

# Returns 0 if the branch is a certified base-fix (caller should certify it).
# A base-fix branch has external_ref=basefail:<name>:<suite> and its gate output
# shows the failing suite as green in the branch trial. Reads _scan_extref from
# the calling land_repo's dynamic scope.
_basefail_fix_check() {  # _basefail_fix_check <id> <gate_out> <gate_suite> <name>
    local _id="$1" _gate_out="$2" _gate_suite="$3" _name="$4" _fse _br_had
    _fse="${_scan_extref[$_id]:-}"
    case "$_fse" in basefail:"$_name":*) : ;; *) return 1 ;; esac
    _fse="${_fse#basefail:$_name:}"
    [ -n "$_fse" ] && [ "$_fse" != "-" ] || return 1
    _br_had="$(printf '%s' "$_gate_out" | awk -v s="$_fse" '
        /^--- this branch/{p=1;next}
        p&&/^(---|gate:)/{p=0}
        p{for(i=1;i<NF;i++) if($i==s&&($(i+1)~/^(RED|TIMEOUT|FAILED)$/||($(i+1)=="was"&&$(i+2)=="killed"))){print "yes";exit}}
    ')"
    [ -z "$_br_had" ]
}

base_incident() {        # base_incident <repo> <suite> <reason> <branch> <base> <gate output>
    local name="$1" suite="$2" reason="$3" br="$4" base="$5" out="$6" id named
    if [ ! -r "$INC" ]; then
        log "CHECK6 $name: no intake at $INC — the base's own red reaches nobody"
        return 1
    fi
    # A gate that named no suite says so IN the bead. `-` alone reads as a formatting fault
    # and sends whoever claims this looking for a field that was never filled in.
    named="$suite"
    [ "$named" = - ] && named="- (the gate named none; read its output below)"
    # THE ID IS THE LAST LINE, NOT THE WHOLE OUTPUT. incident.sh logs through `tee`, so its
    # progress lines share stdout with the id it returns and a bare capture takes both.
    id="$(SPIRA_INCIDENT_TYPE=bug \
          SPIRA_INCIDENT_PRIORITY=1 \
          SPIRA_INCIDENT_ACTOR=landing \
          SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
          SPIRA_INCIDENT_REPO="$name" \
          SPIRA_INCIDENT_REF="basefail:$name:$suite" \
          SPIRA_INCIDENT_CAUSE=base-suite-red \
          bash "$INC" file "$name's own gate fails against $base — nothing can land" - <<PAYLOAD
$name's landing gate was run against $base itself and failed there, so every branch of
this repository is refused for a condition no branch caused. No bead has been reopened and
no attempt charged: the branches are held, and they land on the pass after this is fixed.

  repository       $name
  base             $base
  failing suite    $named
  gate verdict     BASE_FAIL (${reason:-base-red})
  first noticed by $br, which is not at fault
  reproduce        $SPIRA_HOME/gate.sh $base $name

The dedupe key is the repository and the suite, so every other branch blocked by this same
red bumps a recurrence on this bead rather than filing another one.

--- the gate's own output -------------------------------------------------------------
$(printf '%s\n' "$out" | tail -c 6000)
PAYLOAD
)" || { log "CHECK6 $name: the intake could not file the base's red — it stays spooled and drain will retry"; return 1; }
    id="$(printf '%s' "$id" | tail -1 | tr -d '[:space:]')"
    case "$id" in
        "$SPIRA_ID_PREFIX"*) log "CHECK6 $name: the base's own red is $id (suite ${suite:--})" ;;
        *) log "CHECK6 $name: the intake returned no bead id for the base's red — check $INC" ; return 1 ;;
    esac
}

SUBMITTED="$SPIRA_RUN/submitted"
submitted() {            # 0 if nothing more to do for this tip right now
    # rec_n IS LOAD-BEARING even though nothing here reads it. `read` puts every word past
    # the last variable into that variable, so a three-variable read of a four-field record
    # gives rec_state the value "pr 3" — and the `failed` comparison below, which decides
    # whether a failed submission is retried, would then never be true again.
    local id="$1" tip="$2" f="$SUBMITTED/$1" rec_tip rec_at rec_state rec_n
    [ -f "$f" ] || return 1
    read -r rec_tip rec_at rec_state rec_n < "$f" 2>/dev/null || return 1
    [ "$rec_tip" = "$tip" ] || return 1
    [ "$rec_state" = failed ] || return 0
    [ $(( $(date +%s) - rec_at )) -lt 3600 ]
}
# THE FOURTH FIELD IS THE REFRESH BUDGET ALREADY SPENT, and it has to live in the record
# rather than be counted from anywhere else, because a refresh rewrites the branch — which
# moves the tip, which writes a fresh marker. A count derived from anything the refresh
# itself changes resets to zero every time it is spent, and the bound is then no bound.
submitted_rec() {        # submitted_rec <id> state|refreshes
    local f="$SUBMITTED/$1" tip at state n
    [ -f "$f" ] || return 1
    read -r tip at state n < "$f" 2>/dev/null || return 1
    case "$2" in
        state)     printf '%s' "${state:-}" ;;
        refreshes) printf '%s' "${n:-0}" ;;
    esac
}
mark_submitted() {       # mark_submitted <id> <tip> <state> [refreshes]
    mkdir -p "$SUBMITTED"
    printf '%s %s %s %s\n' "$2" "$(date +%s)" "$3" "${4:-0}" > "$SUBMITTED/$1"
}

# ======================================================================================
# A SUBMITTED PULL REQUEST WHOSE BASE HAS MOVED IS DRAGGED BACK ONTO IT.
#
# `pr` mode pushes the branch, opens the pull request and records the tip, and every later
# pass then skips the branch until that tip moves. Nothing moved it. So when the target
# repository's base advances the pull request goes stale, and where a required check tests
# the PR HEAD rather than the merge result it goes red with nobody owning it: the bead is
# closed, the aeon is gone, and Spira has decided it is finished with the branch.
#
# `push` mode never had this. It rebases and merges inside the one pass, so its branch is
# never left standing against a base that can move.
#
# THE PULL REQUEST'S OWN STATE IS ASKED FIRST, and that is not a formality. `gh pr merge
# --squash` lands a NEW commit, so a merged branch is not an ancestor of its base — which
# means the Sending, whose whole predicate is ancestry, never reaps it and it stands here
# forever, further behind with every commit that follows. Without this question every
# merged pull request in the repository would be force-pushed once a pass until its budget
# ran out and then escalated to Ryan as work that would not merge: a page about something
# that finished days ago. A gh that cannot answer is not a licence to rewrite a branch
# either — unreadable is treated as leave it alone.
#
# AND IT IS BOUNDED. A branch refreshed and refreshed that still does not merge is not a
# slow landing, it is a stuck one, and a loop that keeps rebasing it is hiding that rather
# than fixing it. After the cap it is escalated ONCE — the marker's state records that, so
# every later pass is silent — and it stays that way until something moves the branch.
#
# The cap is not a spira.conf key. Nothing about this box's layout sets it; it is a property
# of the mechanism, and the thing an operator tunes is the escalation it produces.
# ======================================================================================
PR_REFRESH_MAX="${SPIRA_PR_REFRESH_MAX:-5}"
PR_REFRESH_N=0           # set by needs_refresh, read by the caller that acts on it

pr_state() {             # pr_state <repo> <branch> -> OPEN|MERGED|CLOSED, non-zero if unknown
    local st
    st="$( cd "$1" && ghq pr view "$2" --json state -q .state 2>/dev/null )"
    [ -n "$st" ] || return 1
    printf '%s' "$st"
}

# needs_refresh — 0 when this already-submitted branch should be rebased, re-gated and
# force-pushed, with the refresh number in PR_REFRESH_N. Non-zero means leave it standing.
needs_refresh() {        # needs_refresh <repo> <name> <branch> <id> <base> <tip>
    local repo="$1" name="$2" br="$3" id="$4" base="$5" tip="$6" st n
    PR_REFRESH_N=0
    # Current already: the base is in the branch, so there is nothing to drag it onto. This
    # is the common answer and it is a local read, which is what keeps this cheap enough to
    # ask about every submitted branch on every pass.
    git -C "$repo" merge-base --is-ancestor "$base" "refs/heads/$br" 2>/dev/null && return 1
    case "$(submitted_rec "$id" state)" in
        stale) log "CHECK6 $id: $br is behind $base and already escalated — leaving it standing"; return 1 ;;
        done)  return 1 ;;
    esac
    st="$(pr_state "$repo" "$br")" || {
        log "CHECK6 $id: $br is behind $base but gh will not say whether its pull request is open — not touching it"
        return 1; }
    if [ "$st" != OPEN ]; then
        log "CHECK6 $id: $br is behind $base but its pull request is $st — nothing to refresh"
        mark_submitted "$id" "$tip" done
        return 1
    fi
    # Normalised to a number before it is compared as one. A marker written by an older
    # harness has three fields, and a truncated write has whatever it has; `[ x -ge 5 ]`
    # against either is a shell error, and the arm it falls to is the one that force-pushes.
    n="$(submitted_rec "$id" refreshes)"
    case "${n:-}" in ''|*[!0-9]*) n=0 ;; esac
    if [ "$n" -ge "$PR_REFRESH_MAX" ]; then
        spira_ask_refresh_loop "$repo" "$name" "$br" "$id" "$base" "$n"
        mark_submitted "$id" "$tip" stale "$n"
        act "escalated $id — its pull request will not merge after $n refresh(es)"
        return 1
    fi
    PR_REFRESH_N=$(( n + 1 ))
    log "CHECK6 $id: $br is behind $base — rebasing its pull request onto it (refresh $PR_REFRESH_N of $PR_REFRESH_MAX)"
    return 0
}

# land_pr <repo> <branch> <id> <base-ref> -> 0 if a pull request is open for this tip.
# The branch is force-pushed with a lease because CHECK 6 rebases it before gating, so the
# remote ref is routinely behind by a rewrite rather than by a divergence — and the lease is
# what keeps that from being a licence to clobber someone else's push.
#
# THE REMOTE AND THE BASE BRANCH BOTH COME OUT OF THE BASE REF. This took `origin` and `main`
# literally, and `--base main` opens a pull request against a branch that does not exist in
# the two repositories whose default is `master`.
land_pr() {
    local repo="$1" br="$2" id="$3" baseref="$4" num title remote base
    remote="$(ref_remote "$baseref")" || remote=origin
    base="$(ref_branch "$baseref")"
    if ! git -C "$repo" push -q --force-with-lease -u "$remote" "$br" 2>/dev/null; then
        log "CHECK6 $id: could not push $br to $remote"
        return 1
    fi
    num="$( cd "$repo" && ghq pr view "$br" --json number -q .number 2>/dev/null )"
    # DOES ANOTHER OPEN PR ALREADY CARRY THIS WORK? A branch is named for its bead, so a
    # successor bead cuts a new branch and this path opens a SECOND pull request for the
    # same commits — which is what happened to sp-pd-ci: #114 carried all nineteen commits
    # of #113 plus one, and both sat open, burning CI minutes and splitting the review.
    # law-decompose-by-deliverable stops the usual cause; this catches the rest, because a
    # duplicate review thread is expensive and silent.
    if [ -z "${num:-}" ]; then
        dup="$( cd "$repo" && ghq pr list --state open --json number,headRefName \
                  -q '.[] | "\(.number) \(.headRefName)"' 2>/dev/null \
                | while read -r n ref; do
                      [ "$ref" = "$br" ] && continue
                      # An existing PR supersedes this branch when it already contains
                      # every commit this branch would add.
                      if [ -z "$(git -C "$repo" log --format='%H' "$base..$br" 2>/dev/null \
                                 | while read -r c; do
                                       git -C "$repo" merge-base --is-ancestor "$c" "origin/$ref" 2>/dev/null || echo x
                                   done)" ]; then
                          echo "$n"; break
                      fi
                  done | head -1 )"
        if [ -n "${dup:-}" ]; then
            log "CHECK6 $id: #$dup already carries every commit on $br — not opening a second pull request"
            bdq note "$id" "Not opening a pull request: #$dup already carries every commit on $br. Continue the review there rather than splitting it across two threads." >/dev/null 2>&1
            return 1
        fi
        title="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
d = d if isinstance(d, list) else [d]
print(d[0].get("title", "") if d else "")' 2>/dev/null)"
        # THE BODY GOES IN ON STDIN, and the heredoc must CLOSE before the `||` arm, or
        # bash reads the arm itself as heredoc content — a redirection is bound to the line
        # it appears on, not to the command that line continues. Prose belongs on stdin
        # anyway: backticks and $( ) in a double-quoted argument are command substitution,
        # and a message that silently loses the terms it was explaining is worse than none
        # (law-commit-messages-via-stdin).
        if ! ( cd "$repo" && ghq pr create --head "$br" --base "$base" \
                 --title "$id: ${title:-Spira}" --body-file - >/dev/null 2>&1 ) <<PRBODY
Filed by Spira for bead $id. The bead is closed in the Spira database; this
pull request is how the work lands, so it is not done until this merges.

Auto-merge is armed — a green run merges it without anyone waiting on it.
PRBODY
        then
            log "CHECK6 $id: gh pr create failed for $br"
            return 1
        fi
        num="$( cd "$repo" && ghq pr view "$br" --json number -q .number 2>/dev/null )"
    fi
    # A green pull request must merge itself. If the repository has auto-merge disabled the
    # arm fails and is worth a line — the pull request is still open and correct, it simply
    # now needs a human, which is the thing to know.
    ( cd "$repo" && ghq pr merge --auto --squash "$br" >/dev/null 2>&1 ) \
        || log "CHECK6 $id: pull request ${num:-?} is open but auto-merge could not be armed"
    log "CHECK6 $id: pull request ${num:-?} open on $br — its CI is the gate now"
    return 0
}

# ======================================================================================
# THE SURVIVORS ARE REBASED THE MOMENT THE BASE MOVES, not when the pass next reaches them.
#
# A landing pass rebases a branch immediately before gating it, so within one turn round the
# loop a branch is always measured against a current base. What it does NOT do is go back:
# a branch this pass has already passed over — most often because the repository's gate tree
# was busy and no verdict was reached — keeps the base it was rebased onto, while the pass
# goes on to land other branches on top of it. It is then stale for as long as it takes the
# next pass to reach it, and every landing in between widens the gap it will have to close:
#
#   20:33  no verdict on <branch> this pass; the next pass takes it      <- rebased, then left
#   ...    five more passes, same answer
#   22:30  reopened <bead> — does not rebase onto the base
#
# Eleven of those in one day and twelve the next, each a finished bead put back on the board
# and a whole agent session spent rebasing. The base had moved under a branch nobody was
# working and nothing brought it forward.
#
# So after a landing, every branch this pass has already judged closed-and-unlanded is
# replayed onto the new base at once. A clean rebase costs a fraction of a second, leaves the
# branch landable on this same pass or the next, and is logged. THIS DOES NOT MAKE A GENUINE
# CONFLICT GO AWAY — a branch and a base that disagree about a file still disagree, and that
# still reopens the bead with the colliding paths named, exactly as before. What it removes is
# the drift a branch accumulates while nothing is looking at it, and it brings the reopen the
# conflict does deserve forward to the landing that caused it, where the note is about one
# commit rather than an hour of them.
#
# THE SET COMES FROM THE LOOP, NOT FROM A FRESH ENUMERATION. Re-deriving it would mean a bead
# query per spira/* ref per landing — after a landing no branch contains the base, so no cheap
# ancestry test filters any of them out — and the loop has already paid for exactly that
# answer. Branches the loop has NOT yet reached need nothing: it rebases each one as it
# arrives at it.
#
# IT IS BOUNDED BY THE LOOP, not by a budget of its own. At worst this replays every survivor
# once per landing, and a survivor is only ever a branch the loop has already gated — so the
# rebases a pass can do this way are bounded by the gates it can do, which the pass budget
# already caps. A clean replay is a fraction of a second; the expensive half of a landing pass
# is the gate, and nothing here runs one.
#
# NEVER UNDER A LIVE AEON, and the check is repeated here rather than inherited from the
# loop. Minutes pass between a branch being judged and a landing that triggers this — a whole
# gate run — and in that window a bead can be reopened elsewhere and claimed. Rewriting
# commits beneath a running aeon destroys work that exists in exactly one place, which is the
# one failure here that nothing can undo.
# ======================================================================================
rebase_survivors() {     # rebase_survivors <repo> <name> <base> <landed-branch> [branch...]
    local repo="$1" name="$2" base="$3" landed="$4" br id tip
    shift 4
    for br in "$@"; do
        [ -n "$br" ] || continue
        [ "$br" = "$landed" ] && continue
        id="${br#spira/}"
        # Already carries the new base: the ordinary answer for every branch after the FIRST
        # landing of a pass has swept them, and it must stay silent or a pass that lands three
        # branches logs the same untouched branch three times.
        git -C "$repo" merge-base --is-ancestor "$base" "refs/heads/$br" 2>/dev/null && continue
        # A ref that has gone since the loop judged it was reaped, landed by hand or slain.
        # Whatever removed it did so deliberately; this holds a list, not a fact
        # (law-absence-needs-a-positive-control — say so rather than fall silent).
        if ! git -C "$repo" show-ref --verify --quiet "refs/heads/$br"; then
            log "CHECK6 $id: $br is gone since this pass judged it — not rebasing it onto $base"
            continue
        fi
        if holder_alive "$id"; then
            log "CHECK6 $id: an aeon took $br while this pass ran — leaving its rebase to it"
            continue
        fi
        # Whoever landed carried this work with them. Rebasing would replay commits whose
        # content is already on the base; the Sending reaps the ref.
        if content_landed "$repo" "$br" "$base"; then
            log "CHECK6 $id: $base now contains every change on $br — nothing left to rebase"
            continue
        fi
        if ! rebase_branch "$br" "$base" "$repo" "$name"; then
            # ONLY A CONFLICT MAY REOPEN, the same rule and the same reason as the loop's own
            # arm: rebase_branch returns 1 four ways and three of them are this pass failing to
            # ask the question rather than an answer to it. Charging those to the work reopens
            # a finished bead as "conflicts in unknown" and costs it an attempt toward poison.
            if [ "${REBASE_FAILURE:-}" != conflict ]; then
                log "CHECK6 $id: could not attempt a rebase of $br onto $base after landing $landed (${REBASE_FAILURE:-unknown}) — not a conflict, leaving the bead closed"
                [ "${REBASE_FAILURE:-}" = rebase-refused ] && \
                    spira_ask_rebase_refused "$id" "$br" "$name" "${REBASE_REFUSED_REASON:-unknown}"
                continue
            fi
            # The squash-and-amend case the content test above cannot see. Only a repository
            # that lands by push reaches this function at all, so today this is always false
            # and always a wasted round trip — kept because it is the loop's arm verbatim, and
            # two routes into a reopen that differ by one check are two routes that will
            # eventually differ by more. It is paid once per conflicting survivor, which is a
            # branch already about to cost a whole session.
            if pr_merged "$repo" "$br"; then
                log "CHECK6 $id: $br does not rebase onto $base, but its pull request is merged — landed, not stuck"
                continue
            fi
            n_swept_conflict=$(( n_swept_conflict + 1 ))
            local _other_beads _reopen_note _rq_n _rn_sweep _cur_br_tip _cur_base_sha _ls_st _ls_tip _ls_at _ls_reason
            _cur_br_tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
            _cur_base_sha="$(git -C "$repo" rev-parse "$base" 2>/dev/null)"
            read -r _ls_st _ls_tip _ls_at _ls_reason <<< "$(land_state "$id" 2>/dev/null || true)"
            if [ "${_ls_st:-}" = RED ] && [ "${_ls_tip:-}" = "$_cur_br_tip" ] && \
               [ "${_ls_reason:-}" = "no-rebase@${_cur_base_sha}" ]; then
                log "CHECK6 $id: tip and base unchanged since last RED mark — skipping duplicate bump"
                continue
            fi
            _rn_sweep="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo '?')"
            _other_beads="$(other_beads_on_conflicts "$repo" "$br" "$base" "${REBASE_CONFLICTS:-}")"
            _reopen_note="Reopened by sentinel: $br does not rebase onto $base in $name after $landed landed; conflicts in ${REBASE_CONFLICTS:-unknown}. The branch carries $_rn_sweep commit(s) from the previous session — resume from the existing work."
            if [ -n "$_other_beads" ]; then
                _reopen_note="$_reopen_note Those files were changed on $base by $_other_beads — check whether this work is already landed before resolving."
            else
                _reopen_note="$_reopen_note A merge conflict is not an escalation — the next aeon is handed the rebase and must resolve it."
            fi
            bump_requeue "$id" merge-conflict >/dev/null 2>&1
            _rq_n="$(requeues_of "$id")"
            if [ "${_rq_n:-0}" -ge "${SPIRA_REBASE_ESCALATE_AT:-3}" ]; then
                spira_ask_rebase_loop "$id" "$br" "$name" "$_rq_n" "${REBASE_CONFLICTS:-unknown}" "$_other_beads"
                progress "escalated $id — rebase conflict x${_rq_n} on $br"
            else
                bead_reopen "$id" rebase-conflict "$_reopen_note"
                progress "reopened $id — does not rebase onto $base"
                spira_event bead.reopened "$id" "reopened $id — $br does not rebase onto $base in $name" \
                    "conflicts in ${REBASE_CONFLICTS:-unknown}; the next aeon is handed the rebase" || true
            fi
            land_mark "$id" RED "$_cur_br_tip" "no-rebase@${_cur_base_sha}"
            continue
        fi
        n_swept=$(( n_swept + 1 ))
        tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
        # RECORDED, because the pair of counters in the pass-complete line is the only
        # evidence of whether this is worth doing: a sweep that only ever conflicts is a
        # sweep that has moved the reopen earlier and saved nobody anything, and that is a
        # fact the log should be able to settle without new machinery
        # (law-take-the-simple-fix-with-a-meter).
        land_mark "$id" REBASED "$tip" swept
        log "CHECK6 $id: rebased $br onto $base after landing $landed — still landable"
    done
}

land_repo() {
    local name="$1" repo br id st mode base land tip merged pushed nothing wedged attempt brs refresh
    local bead_repo_name bead_repo_path gate_out base_branch base_remote bead_labels
    local norebase was _ref _obj gate_suite basefail_filed= _cur_st _budget_cut=0
    local -a _cert_brs=() _cert_beadids=() _cert_tips=()
    local -A enum_tip=()
    # WHAT THIS PASS HAS ALREADY JUDGED CLOSED, REBASED AND STILL UNLANDED. A branch enters
    # when its rebase onto the base succeeds and leaves the moment it stops being that — it
    # landed, or it went back on the board. rebase_survivors replays whatever is left after
    # each landing, so a branch the pass has walked past does not sit behind a base that
    # moved under it until some later pass happens to reach it.
    local -A judged=()
    repo="$(repo_root "$name")" || { log "CHECK6 $name: no repo-map entry — skipped"; return 0; }
    [ -e "$repo/.git" ] || { log "CHECK6 $name: $repo is not a git checkout — skipped"; return 0; }

    # The local ref read comes first and the fetch is paid for only if there is something to
    # land. This runs every two minutes across every registered repository; an unconditional
    # fetch of each would be thousands of round trips a day to learn nothing.
    #
    # THE TIP IS READ WITH THE NAME, and it is what makes a vanished branch answerable. By
    # the time this loop reaches a ref that has been reaped the ref is gone, so nothing can
    # be asked about it any more — not "was this landed", not even "what was it". Held from
    # the enumeration, the commit outlives the ref (it is on the base, which is why the ref
    # was reaped) and the pass can say which of the two reasons it disappeared for.
    brs="$(git -C "$repo" for-each-ref --format='%(refname:short) %(objectname)' 'refs/heads/spira/*' 2>/dev/null)"
    [ -n "$brs" ] || return 0
    n_branches=$(( n_branches + $(printf '%s\n' "$brs" | grep -c . || true) ))
    while read -r _ref _obj; do
        [ -n "${_ref:-}" ] && enum_tip["$_ref"]="$_obj"
    done <<< "$brs"
    brs="$(printf '%s\n' "$brs" | awk 'NF{print $1}')"

    mode="$(repo_land "$name")"

    # THE BASE IS RESOLVED BEFORE THE FETCH, AND THE FETCH FOLLOWS IT. `git fetch origin` was
    # literal here; a remote need not be called `origin`, so that fetch was a silent no-op in the
    # one repository whose refs nothing else in this harness touches. Resolving first is safe
    # because the ANSWER is a ref name, which a fetch does not change — only what the ref
    # points at, which is why the fetch still has to happen before anything is measured.
    #
    # A repository whose base cannot be established is SKIPPED, loudly. Every alternative is
    # worse: rebasing onto a branch that does not exist reopens finished work with a reason
    # that is not a reason, and merging into a guessed branch writes to one nobody chose.
    base="$(spira_landref "$repo")" || {
        log "CHECK6 $name: cannot resolve the ref its branches land on — skipped. Give it a \`base\` in repo-map."
        return 0; }
    base_branch="$(ref_branch "$base")"
    base_remote="$(ref_remote "$base")" || base_remote=""
    [ -n "$base_remote" ] && git -C "$repo" fetch -q --no-write-fetch-head "$base_remote" 2>/dev/null
    land="$SPIRA_RUN/worktree/.landing.$(basename "$repo")"
    if [ "$mode" = push ]; then
        if [ ! -e "$land/.git" ]; then
            mkdir -p "$(dirname "$land")"
            # Through the chokepoint. This runs on every landing pass, every two minutes,
            # over every repository — so it is the prune most likely to be the one standing
            # over a live aeon's tree when that tree's `.git` link is momentarily unreadable.
            spira_prune_worktrees "$repo" >/dev/null 2>&1
            git -C "$repo" worktree add -q --detach "$land" "$base" 2>/dev/null || true
        fi
        [ -e "$land/.git" ] && git -C "$land" checkout -q -B landing "$base" 2>/dev/null
    fi

    # ==================================================================================
    # THE SCAN: one query for every branch, not one per branch. A bdjson show per branch
    # was 466 ms each, growing linearly with the unlanded count; a single show with every
    # id is the same answer once. At 25 branches that is ~11.7 s of per-branch queries
    # replaced by ~0.5 s of one bulk query.
    #
    # A branch whose id is absent from the map is treated exactly as a bdjson show that
    # returned nothing is treated today: st is empty, and the loop skips it through the
    # non-closed path. The repo: label still falls back to the repository being swept.
    #
    # ONE QUERY FOR THE SWEEP IS NOT ONE QUERY FOR THE LANDING. A branch that passes the
    # scan and makes it through the gate is re-read individually before landing or
    # reopening — the map is up to a pass old by then, and closing or reopening on a stale
    # status is how work gets reopened that already landed (law-closed-is-not-landed).
    # ==================================================================================
    local -A _scan_st=() _scan_repo=() _scan_labels=() _scan_superseded=() _scan_closed_at=() _scan_priority=() _scan_extref=()
    local _scan_ids=""
    for br in $brs; do
        _scan_ids="$_scan_ids ${br#spira/}"
    done
    if [ -n "${_scan_ids// /}" ]; then
        local _sid _sst _srepo _ssup _scat _spri _sextref _slabels
        # shellcheck disable=SC2086
        while IFS=$'\t' read -r _sid _sst _srepo _ssup _scat _spri _sextref _slabels; do
            [ -n "${_sid:-}" ] || continue
            _scan_st["$_sid"]="$_sst"
            _scan_repo["$_sid"]="$_srepo"
            _scan_labels["$_sid"]="$_slabels"
            _scan_superseded["$_sid"]="${_ssup:-0}"
            _scan_closed_at["$_sid"]="${_scat:-}"
            _scan_priority["$_sid"]="${_spri:-9999}"
            _scan_extref["$_sid"]="${_sextref:-}"
        done < <(bdjson show $_scan_ids 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit
d = d if isinstance(d, list) else [d]
home = sys.argv[1]
for i in d:
    bid = i.get("id", "")
    if not bid: continue
    st = i.get("status", "-")
    repo = next((l[5:] for l in (i.get("labels") or []) if l.startswith("repo:")), home)
    labels = " ".join(i.get("labels") or [])
    # `bd list` and `bd show` name the supersession field differently: show returns
    # "dependency_type", list returns "type". Accept either spelling (law-absent-needs-a-positive-control
    # was triggered by this exact bug — sp-dvlq was superseded by sp-35pl and was still reopened
    # every two minutes because only the show spelling was read off a list row).
    sup = 1 if any((x.get("dependency_type") or x.get("type")) == "supersedes"
                   for x in (i.get("dependencies") or [])) else 0
    # cat and pri BEFORE extref BEFORE labels: IFS=$'\t' collapses consecutive tabs (tab is
    # IFS-whitespace), so any empty field before a non-empty one shifts the read variables.
    # extref uses "-" as a sentinel for absent so it is never empty; labels is the only field
    # that may safely be empty (and trailing). cat uses a high-sorting sentinel for non-closed
    # beads so they sort after all closed branches.
    cat = i.get("closed_at") or "9999-99-99"
    pri = i.get("priority") if i.get("priority") is not None else 9999
    extref = i.get("external_ref") or "-"
    print(f"{bid}\t{st}\t{repo}\t{sup}\t{cat}\t{pri}\t{extref}\t{labels}")
' "$(spira_home_repo)" 2>/dev/null)
    fi
    # Base-fix branches (external_ref=basefail:<name>:*) sort before all others so a
    # budget cut cannot defer the fix that unblocks every held branch. Within each group,
    # certify oldest-closed first within each priority tier so no branch starves.
    local _fix_front=""
    brs="$(
        for _br in $brs; do
            _id="${_br#spira/}"
            _fk=1; case "${_scan_extref[$_id]:-}" in basefail:"$name":*) _fk=0 ;; esac
            printf '%s\t%s\t%s\t%s\n' "$_fk" \
                "${_scan_priority[$_id]:-9999}" \
                "${_scan_closed_at[$_id]:-9999-99-99}" "$_br"
        done | sort -t$'\t' -k1,1n -k2,2n -k3,3 | awk -F'\t' '{print $4}'
    )"
    for _br in $brs; do
        case "${_scan_extref[${_br#spira/}]:-}" in
            basefail:"$name":*) _fix_front="${_fix_front:+$_fix_front }$_br" ;;
            *) break ;;
        esac
    done
    [ -n "$_fix_front" ] && log "CHECK6 $name: base-fix branch(es) at front of queue: $_fix_front"

    # EXPRESS FIRST: branches whose beads carry the express label are certified before
    # the rest so a critical bead does not wait behind alphabetical refname order.
    local _exp_label_land="${SPIRA_EXPRESS_LABEL:-express}"
    local _expr_brs="" _tail_brs="" _br_sort
    for _br_sort in $brs; do
        _bid_sort="${_br_sort#spira/}"
        case " ${_scan_labels[$_bid_sort]:-} " in
            *" $_exp_label_land "*) _expr_brs="$_expr_brs $_br_sort" ;;
            *) _tail_brs="$_tail_brs $_br_sort" ;;
        esac
    done
    [ -n "$_expr_brs" ] && \
        log "CHECK6 $name: express branch(es) certified first:${_expr_brs}"
    brs="${_expr_brs# }${_tail_brs:+ }${_tail_brs# }"

    for br in $brs; do
        id="${br#spira/}"
        _land_state "repo=$name" "branch=$br"
        # THE LIST IS OLDER THAN THE LOOP. `brs` was read once at the top of this function
        # and a pass legitimately runs for tens of minutes — the 14:42 pass on 2026-09-07
        # reached its last branch at 15:16. In that window a branch can be landed by hand,
        # reaped, or deleted by a slaying, and `rebase_branch` against a ref that no longer
        # exists fails exactly like a conflict does. That reopened sp-fmd5 thirteen minutes
        # after its two commits were merged into origin/main and its branch deleted:
        # finished work put back on the board, to be claimed and redone by the next aeon.
        #
        # A VANISHED BRANCH IS NEVER EVIDENCE OF UNLANDED WORK. Whatever removed it did so
        # deliberately; this pass simply holds a stale list. Skip it and say so — silence
        # here would make a re-read indistinguishable from a branch that was never seen.
        #
        # AND IT SAYS WHICH, rather than "landed or reaped elsewhere" — a line that names
        # both possibilities settles neither, and this is the one place a reader looks when
        # asking whether work was lost. The tip held from the enumeration answers it: on the
        # base means the Sending reaped a landed branch, which is the ordinary case and needs
        # no attention; not on the base means something removed work that is nowhere else,
        # which is the case worth seeing (law-absence-needs-a-positive-control).
        if ! git -C "$repo" show-ref --verify --quiet "refs/heads/$br"; then
            was="${enum_tip[$br]:-}"
            if [ -n "$was" ] && git -C "$repo" merge-base --is-ancestor "$was" "$base" 2>/dev/null; then
                log "CHECK6 $id: $br is gone since this pass began and $was is on $base — landed and reaped, not reopening"
            elif [ -n "$was" ]; then
                log "CHECK6 $id: $br is gone since this pass began and $was is NOT on $base — reaped or slain, not reopening"
            else
                log "CHECK6 $id: $br is gone since this pass began — landed or reaped elsewhere, not reopening"
            fi
            continue
        fi
        st="${_scan_st[$id]:-}"
        bead_repo_name="${_scan_repo[$id]:-}"
        bead_labels="${_scan_labels[$id]:-}"
        bead_superseded="${_scan_superseded[$id]:-0}"
        # A BRANCH SKIPPED FOR A BEAD THAT IS NOT CLOSED HAS TO HAVE A VOICE. This was a bare
        # `continue`, so the one state that most needs saying — a branch whose bead sits
        # in_progress while nothing is holding it — left no trace anywhere in this log, and
        # the only witness was a KEEP line from the reaper that reads identically to work
        # legitimately in flight. Absence and health looked the same
        # (law-absence-needs-a-positive-control).
        #
        # ONCE PER BRANCH PER PASS, which is what this loop already gives: the noise floor is
        # one line per unlanded branch every pass, and the distinction that makes it worth
        # reading is whether anybody is home. A live holder is ordinary; no holder on an
        # in_progress bead is a lease nobody is working, and it is named as such.
        if [ "${st:-}" != "closed" ]; then
            if holder_alive "$id"; then
                log "CHECK6 $id: $br not landed — its bead is ${st:--}, held by a live aeon"
            else
                log "CHECK6 $id: $br not landed — its bead is ${st:--} and no aeon holds it"
            fi
            continue
        fi

        # THE BRANCH BEING HERE IS NOT EVIDENCE THAT IT BELONGS HERE. A branch is only landed
        # in the repository its BEAD names; a ref that says otherwise is a bug elsewhere, and
        # merging it anyway would push one repository's work into another repository's main —
        # the silent wrong-repository failure this whole change exists to make impossible. It
        # must not be talked out of that by a plausible-looking ref.
        bead_repo_path="$(repo_root "${bead_repo_name:-}")" || bead_repo_path=""
        if [ "$bead_repo_path" != "$repo" ]; then
            log "CHECK6 $id: $br is in $name but the bead names repo:${bead_repo_name:-?} — not landing it here"
            continue
        fi
        # A SUPERSEDED BEAD'S BRANCH WILL NEVER LAND HERE. `bd supersede` records the
        # relation as a `supersedes` dependency; its work was carried onto the successor's
        # branch and landed under the successor's id. Rebasing would produce a conflict
        # BECAUSE the base already holds those changes, and reopening says "closed without
        # landing" about work that is already there. The Sending reaps the branch; this pass
        # leaves the bead alone. The exemption is the same one aeon.sh carries for its verdict
        # check and the sentinel carries for CHECK 5 — the three must answer identically.
        if [ "${bead_superseded:-0}" = 1 ]; then
            log "CHECK6 $id: $br is superseded — its work landed under the successor's id; leaving it for the Sending to reap"
            continue
        fi
        # WHETHER THE BASE ALREADY HOLDS THIS WORK IS THE WHOLE QUESTION, and ancestry is
        # only one of the two ways the answer is yes. A branch already merged has nothing to
        # land; re-merging it is a no-op that still logs an ACT, and that re-landed
        # spira/sp-stranded on every pass for twenty minutes. Reaping the ref is CHECK 6b's
        # job, not this one's.
        #
        # A SQUASHING REPOSITORY NEVER MAKES THE BRANCH AN ANCESTOR. `pr` mode arms
        # --squash, so GitHub lands the work as one new commit the branch is not in the
        # history of — the ancestry test then says "not landed" about work sitting on the
        # base, the rebase below conflicts BECAUSE the base already holds those changes, and
        # a finished bead is reopened on the strength of the pair. content_landed asks
        # whether merging would change anything, which is the question that survives a
        # rewrite of the commits.
        if content_landed "$repo" "$br" "$base"; then
            log "$base already contains every change on $br — nothing to land"
            # RECORD THIS PATH so sending.sh's landstate assertion does not fire for a
            # branch landing.sh legitimately skipped. Without this, every content-reaped
            # branch would look identical to the sp-qj8n shape (no landstate entry at all)
            # and the assertion would fire for ordinary squash landings and review-only
            # beads that landing.sh correctly determined needed no push.
            tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
            land_mark "$id" CONTENT "${tip:-none}"
            rm -f "$LANDSTATE/$id.ejected" 2>/dev/null || true
            continue
        fi

        # NEVER REBASE UNDER A LIVE AEON. Rebasing rewrites the commits beneath a working
        # tree, so doing it while an aeon is inside destroys work that exists in exactly one
        # place. The bead is closed here, which SHOULD mean nobody is home — "should" is not
        # "is", the aeon runs on past its close to its own verdict step, and the check costs
        # one /proc read. If someone is home, this branch is simply landed on the next pass.
        if holder_alive "$id"; then
            log "CHECK6 $id: a live aeon still holds $br — deferring the land"
            continue
        fi

        # A branch already sent under a mode that leaves it standing is not re-sent. Checked
        # before the rebase, because rebasing an open pull request's branch on every pass
        # would rewrite it under its own reviewer — and the ONE thing that overrides that is
        # a base which has moved out from under the pull request, which is the case
        # needs_refresh isolates. A `hold`-mode branch has no pull request to go stale and
        # no remote to push to, so it is only ever left alone.
        tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
        refresh=0
        if [ "$mode" != push ] && submitted "$id" "$tip"; then
            if [ "$mode" != pr ] || ! needs_refresh "$repo" "$name" "$br" "$id" "$base" "$tip"; then
                continue
            fi
            refresh="$PR_REFRESH_N"
        fi

        if ! rebase_branch "$br" "$base" "$repo" "$name"; then
            # ONLY A CONFLICT MAY REOPEN. rebase_branch returns 1 for four different things
            # and exactly one of them is a fact about the branch; the other three are the
            # pass failing to ask the question — most often a ref reaped out from under a
            # branch list this loop read minutes ago, which is precisely the state the check
            # above is racing and cannot win outright. Charging those to the work reopens a
            # finished bead as "conflicts in unknown", costs an aeon a session finding
            # nothing to rebase, and counts against the bead toward poison.
            if [ "${REBASE_FAILURE:-}" != conflict ]; then
                log "CHECK6 $id: could not attempt a rebase of $br onto $base (${REBASE_FAILURE:-unknown}) — not a conflict, leaving the bead closed"
                [ "${REBASE_FAILURE:-}" = rebase-refused ] && \
                    spira_ask_rebase_refused "$id" "$br" "$name" "${REBASE_REFUSED_REASON:-unknown}"
                continue
            fi
            # "DOES NOT REBASE" IS NOT EVIDENCE OF UNLANDED WORK ON ITS OWN. content_landed
            # above has already cleared the ordinary squash case; this catches the one it
            # cannot — a squash that merged and was then amended on the base, where the
            # content genuinely differs and re-landing the branch would revert the amendment.
            # The network call sits here, behind a cheap check that has already failed, so it
            # is paid for only by a branch that is about to be reopened.
            if pr_merged "$repo" "$br"; then
                log "CHECK6 $id: $br does not rebase onto $base, but its pull request is merged — landed, not stuck"
                continue
            fi
            local _other_beads _reopen_note _rq_n _cur_base_sha _ls_st _ls_tip _ls_at _ls_reason
            _cur_base_sha="$(git -C "$repo" rev-parse "$base" 2>/dev/null)"
            read -r _ls_st _ls_tip _ls_at _ls_reason <<< "$(land_state "$id" 2>/dev/null || true)"
            if [ "${_ls_st:-}" = RED ] && [ "${_ls_tip:-}" = "$tip" ] && \
               [ "${_ls_reason:-}" = "no-rebase@${_cur_base_sha}" ]; then
                log "CHECK6 $id: tip and base unchanged since last RED mark — skipping duplicate bump"
                continue
            fi
            _reopen_note="$(conflict_reopen_note "$repo" "$br" "$base" "$name" "${REBASE_CONFLICTS:-}" "sentinel")"
            _other_beads="$(other_beads_on_conflicts "$repo" "$br" "$base" "${REBASE_CONFLICTS:-}")"
            bump_requeue "$id" merge-conflict >/dev/null 2>&1
            _rq_n="$(requeues_of "$id")"
            if [ "${_rq_n:-0}" -ge "${SPIRA_REBASE_ESCALATE_AT:-3}" ]; then
                spira_ask_rebase_loop "$id" "$br" "$name" "$_rq_n" "${REBASE_CONFLICTS:-unknown}" "$_other_beads"
                progress "escalated $id — rebase conflict x${_rq_n} on $br"
            else
                bead_reopen "$id" rebase-conflict "$_reopen_note"
                progress "reopened $id — does not rebase onto $base"
                spira_event bead.reopened "$id" "reopened $id — $br does not rebase onto $base in $name" \
                    "conflicts in ${REBASE_CONFLICTS:-unknown}; the next aeon is handed the rebase" || true
            fi
            land_mark "$id" RED "$tip" "no-rebase@${_cur_base_sha}"
            continue
        fi
        tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
        judged["$br"]=1

        # QUEUE MODE: gate before certifying so fence violations are caught per-branch
        # and never reach a batch PR or CI where they are unattributable (sp-hm2vw).
        #
        # SPIRA_CERTIFY_SUITES=off KEEPS THE FENCES AND DROPS THE SUITES. Every branch still
        # gets bash -n, the no-beads-data check and the repository's fence commands; the
        # suites are left to the batch's CI run, which runs them anyway on a runner fleet
        # that was sitting idle while this box ran them first. 2026-09-23: thirty closed
        # beads in 48 hours were RED here (thirteen of them `timeout`) and none reached a
        # batch — including sp-a5jpo, the fix for exactly this (per Ryan: "we're running tests
        # locally, holding up those beads. then we'll batch them … to be tested again").
        # Push mode (below) lands straight on the base and keeps the full gate.
        # With certify_pass_par > 1, branches are collected here and gated in parallel
        # after the loop; with certify_pass_par == 1, the serial path runs inline.
        if [ "$mode" = queue ]; then
            # IDLE-SKIP (sp-a5jpo): when the cert queue is empty and CI is idle, this
            # branch would be the sole batch member. At batch size 1 the bisect argument
            # for per-branch certification is vacuous — CI runs the same work anyway. Skip
            # the local gate and certify directly; the CI gate is the only authority.
            # cert queue is checked first (local file reads) so the forge call is only
            # made when there is actually something to skip.
            if [ "${SPIRA_CERT_IDLE_SKIP:-1}" = 1 ]; then
                _cq_n="$(queue_certified_list "$repo" 2>/dev/null | grep -c . || true)"
                if [ "${_cq_n:-1}" -eq 0 ]; then
                    _ci_act="$("${SPIRA_FORGE:-$SPIRA_HOME/forge.sh}" runs-active "$repo" 2>/dev/null)" || _ci_act="?"
                    if [ "${_ci_act:-?}" = 0 ]; then
                        _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
                        if [ "${_cur_st:-}" = "closed" ]; then
                            log "CHECK6 $id: CI idle, sole batch member — skipping certification gate"
                            land_mark "$id" CERTIFIED "$tip"
                            mark_submitted "$id" "$tip" certified
                            progress "certified $br in $name — CI idle, sole batch member"
                        else
                            log "CHECK6 $id: bead is now ${_cur_st:--} (was closed at scan time) — not certifying $br"
                        fi
                        continue
                    fi
                fi
            fi
            if [ "${certify_pass_par:-1}" -le 1 ]; then
                if ! gate_fits; then
                    case "${_scan_extref[$id]:-}" in
                        basefail:"$name":*)
                            log "CHECK6 $id: base-fix branch — gating despite budget exhaustion" ;;
                        *)
                            _budget_cut=1; break ;;
                    esac
                fi
                _land_state "repo=$name" "branch=$br" "phase=gate"
                gate_lock_wait
                gate_out="$(SPIRA_GATE_LOCK_WAIT="$_gate_wait" SPIRA_GATE_BEAD="$id" \
                    SPIRA_GATE_SUITES="${SPIRA_CERTIFY_SUITES:-on}" \
                    "$SPIRA_HOME/gate.sh" "$br" "$name" 2>&1)"
                gate_rc=$?
                _land_state "repo=$name" "branch=$br"
                gate_outcome="$(spira_gate_outcome "$gate_rc")"
                gate_reason="$(printf '%s' "$gate_out" \
                    | sed -n 's/^gate: VERDICT=[A-Z_]* reason=\([^ ]*\).*$/\1/p' | tail -1)"
                gate_suite="$(printf '%s' "$gate_out" \
                    | sed -n 's/^gate: VERDICT=.* suite=\([^ ]*\).*$/\1/p' | tail -1)"
                [ -n "$gate_suite" ] || gate_suite=-
                if [ "$gate_rc" -ne 0 ]; then
                    log "CHECK6 $id: certification gate $gate_outcome on $br in $name (${gate_reason:-unspecified})"
                    land_mark "$id" GATED "$tip" "$gate_outcome:${gate_reason:-unspecified}"
                    if [ "$gate_rc" = "$SPIRA_GATE_BASEFAIL" ]; then
                        log "CHECK6 $id: held — the base fails its own gate (suite $gate_suite)"
                        if _basefail_fix_check "$id" "$gate_out" "$gate_suite" "$name"; then
                            local _fse_cert="${_scan_extref[$id]:-}"
                            _fse_cert="${_fse_cert#basefail:$name:}"
                            if [ "${_scan_st[$id]:-}" = "closed" ]; then
                                log "CHECK6 $id: base-fix: $br is green on $name's red suite $_fse_cert — certifying"
                                land_mark "$id" CERTIFIED "$tip"
                                mark_submitted "$id" "$tip" certified
                                progress "certified $br in $name — base-fix (suite $_fse_cert)"
                            fi
                        else
                            if [ "${basefail_filed:-}" != 1 ]; then
                                basefail_filed=1
                                base_incident "$name" "$gate_suite" "${gate_reason:-base-red}" \
                                              "$br" "$base" "$gate_out"
                            fi
                        fi
                        if _basefail_fix_check "$id" "$gate_out" "$gate_suite" "$name"; then
                            local _fse_cert="${_scan_extref[$id]:-}"
                            _fse_cert="${_fse_cert#basefail:$name:}"
                            _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
                            if [ "${_cur_st:-}" = "closed" ]; then
                                log "CHECK6 $id: base-fix: $br is green on $name's red suite $_fse_cert — certifying"
                                land_mark "$id" CERTIFIED "$tip"
                                mark_submitted "$id" "$tip" certified
                                progress "certified $br in $name — base-fix (suite $_fse_cert)"
                            fi
                        fi
                        continue
                    fi
                    if ! spira_gate_blames_branch "$gate_rc"; then
                        nv_key="$(printf '%s' "$br-${gate_reason:-unspecified}" | tr -c 'A-Za-z0-9._-' '-')"
                        nv_file="$SPIRA_RUN/noverdict/$nv_key"
                        mkdir -p "$SPIRA_RUN/noverdict"
                        nv_n=$(( $(cat "$nv_file" 2>/dev/null || echo 0) + 1 ))
                        printf '%s\n' "$nv_n" > "$nv_file"
                        if [ "$nv_n" -ge "${SPIRA_NOVERDICT_MAX:-3}" ] && [ ! -e "$nv_file.asked" ]; then
                            : > "$nv_file.asked"
                            spira_ask_machinery "$id" "$br" "$name" "$gate_outcome" "$gate_reason" "$nv_n" "$gate_out"
                            progress "escalated $id — $gate_outcome x$nv_n on $br"
                        fi
                        [ "${gate_reason:-}" = timeout ] && land_mark "$id" RED "$tip" timeout
                        continue
                    fi
                    _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
                    if [ "${_cur_st:-}" != "closed" ]; then
                        log "CHECK6 $id: bead is now ${_cur_st:--} (was closed at scan time) — not reopening $br"
                        continue
                    fi
                    local _rn_cert
                    _rn_cert="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo '?')"
                    bead_reopen "$id" cert-gate-red "Reopened by sentinel: branch $br failed $name's certification gate. The branch carries $_rn_cert commit(s) from the previous session — the next aeon should resume from the existing work, not restart.

$(printf '%s' "$gate_out" | tail -20)"
                    unset _rn_cert
                    progress "reopened $id — failed the certification gate"
                    spira_event bead.reopened "$id" "reopened $id — $br failed $name's certification gate" \
                        "$(printf '%s' "$gate_out" | tail -3)" || true
                    land_mark "$id" RED "$tip" gate
                    continue
                fi
                [ "${gate_reason:-}" = cached ] \
                    && log "CHECK6 $id: certification PASS on $br in $name — this tree had already passed"
                rm -f "$SPIRA_RUN/noverdict/$(printf '%s' "$br" | tr -c 'A-Za-z0-9._-' '-')"* 2>/dev/null
                _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
                if [ "${_cur_st:-}" != "closed" ]; then
                    log "CHECK6 $id: bead is now ${_cur_st:--} (was closed at scan time) — not certifying $br"
                    continue
                fi
                land_mark "$id" CERTIFIED "$tip"
                mark_submitted "$id" "$tip" certified
                progress "certified $br in $name — queued"
                continue
            fi
            # Parallel path: defer gating to Phase 2 after the loop.
            # Budget is checked here so a tight pass cuts in Phase 1 and the
            # cut message fires at the same point it does on the serial path.
            if ! gate_fits; then
                case "${_scan_extref[$id]:-}" in
                    basefail:"$name":*)
                        log "CHECK6 $id: base-fix branch — gating despite budget exhaustion" ;;
                    *)
                        _budget_cut=1; break ;;
                esac
            fi
            _cert_brs+=("$br"); _cert_beadids+=("$id"); _cert_tips+=("$tip")
            continue
        fi

        # CONFINEMENT COMES BEFORE THE GATE. A spike's branch may pass every test in the
        # repository and still be the wrong thing to merge — its experiment compiles, which
        # is the point of an experiment. This asks a different question from the gate ("is
        # this branch allowed to land at all") and it must be asked first, because the gate
        # is the expensive half and there is nothing to learn from running it on a branch
        # that is going back either way. A bead that is not a spike passes through untouched.
        gate_out="$("$SPIRA_HOME/confine.sh" "$id" "$br" "$repo" "$base" "${bead_labels:-}" 2>&1)"
        confine_rc=$?
        if [ "$confine_rc" = 1 ]; then
            bead_reopen "$id" confine-fail "Reopened by sentinel: $gate_out"
            progress "reopened $id — spike branch is not confined to its document"
            log "CHECK6 $id: $(printf '%s' "$gate_out" | head -1)"
            # RED, LIKE ANY OTHER FAULT OF THE BRANCH'S OWN. A refusal here is the branch
            # being wrong rather than the repository or the pass being busy, so it belongs in
            # the record beside the rebase failure and the gate failure. The stretch between
            # DONE and LANDED is the one with no witness, and a branch that stops for good in
            # the middle of it is exactly the case that record exists to make visible.
            land_mark "$id" RED "$tip" confine
            unset 'judged[$br]'
            continue
        elif [ "$confine_rc" != 0 ]; then
            # confine.sh could not complete its evaluation — library load failure, database
            # unreachable, or similar infrastructure fault. Same treatment as a live aeon still
            # holding the branch (above): log the cause and defer to the next pass. The branch
            # has NOT been found in violation; do not reopen and do not mark RED.
            log "CHECK6 $id: $br — confine.sh could not evaluate: $(printf '%s' "$gate_out" | head -1)"
            continue
        fi

        # THE NOTE CARRIES THE GATE'S OWN WORDS. A bead reopened with "failed the landing
        # gate" tells its next aeon nothing it can act on, and after three of those the bead
        # poisons and reaches the operator with a reason that is not a reason. The gate already
        # distinguishes a branch's own fault from a repository whose gate fails against its
        # base; that distinction is worthless if it stops at a log nobody reads.
        # The budget check sits HERE, immediately before the only expensive call in the
        # loop, rather than at the top of the pass: everything above is cheap, and a branch
        # that needs no gate should still be processed in the tail of a pass.
        if ! gate_fits; then
            case "${_scan_extref[$id]:-}" in
                basefail:"$name":*)
                    log "CHECK6 $id: base-fix branch — gating despite budget exhaustion" ;;
                *)
                    _budget_cut=1; break ;;
            esac
        fi
        # THE BEAD IS NAMED TO THE GATE, because this pass is the only caller that knows it
        # for certain. The gate's yield record otherwise derives the bead from the branch
        # name, which is right only while a branch is named after the bead it was cut for —
        # and a branch's affinity is recorded precisely because that is not always true
        # (law-branch-affinity-is-recorded).
        _land_state "repo=$name" "branch=$br" "phase=gate"
        gate_lock_wait
        gate_out="$(SPIRA_GATE_LOCK_WAIT="$_gate_wait" SPIRA_GATE_BEAD="$id" \
            "$SPIRA_HOME/gate.sh" "$br" "$name" 2>&1)"
        gate_rc=$?
        _land_state "repo=$name" "branch=$br"
        # ------------------------------------------------------------------------------
        # FOUR OUTCOMES, AND ONLY ONE OF THEM IS THE BRANCH'S FAULT (conf.sh).
        #
        # Nothing lands on any of the three non-PASS outcomes — the gate fails closed and
        # that is not up for negotiation. What differs is who is CHARGED, and that used to be
        # decided by "non-zero", so a lock, a deadline, a missing worktree and a repository
        # whose suites fail on its own base all reopened the bead saying the branch failed the
        # gate. Three of those poison it and page the operator about work that was fine; it
        # happened five times on 2026-09-06 alone (sp-d21), and again all morning today.
        #
        # `spira_gate_blames_branch` is the one place that decides, so the landing pass, the
        # sentinel and any future caller cannot drift apart on it.
        #
        # THREE ARMS BELOW, BECAUSE "NOT THE BRANCH'S FAULT" IS NOT ONE ANSWER. A BASE_FAIL
        # has an owner — the repository whose gate is red against its own base — and it is
        # filed as work for whoever can change that code. A NO_VERDICT has none: nobody can
        # be handed a lock or a deadline, so it is counted and, if it keeps recurring, put in
        # front of the operator. Only a FAIL reopens the bead.
        #
        # HOW A REPOSITORY'S OWN GATE COMMAND MAPS INTO THESE OUTCOMES. The gate command
        # in the repo-map is the repository's contract with this pass; its non-zero exit
        # codes must land in the right outcome. For spira, testenv-batch.sh exits 2 when
        # the container fails to start and 3 when installation inside it fails — both are
        # harness faults, not branch faults. The gate command maps both to exit 75 (NO_VERDICT)
        # before returning, so neither charges the branch an attempt.
        # ------------------------------------------------------------------------------
        gate_outcome="$(spira_gate_outcome "$gate_rc")"
        # The gate's own machine-readable line, when it produced one. Read anchored, so a
        # reword of the prose around it cannot quietly turn every verdict into "unknown".
        gate_reason="$(printf '%s' "$gate_out" \
            | sed -n 's/^gate: VERDICT=[A-Z_]* reason=\([^ ]*\).*$/\1/p' | tail -1)"
        # The suite the repository's own gate named, for the incident's dedupe key. Read from
        # the same anchored line and never from the prose around it: the key has to survive a
        # reword, or one broken base files a fresh bead every pass. `-` when the gate named
        # none, which is a stable key too.
        gate_suite="$(printf '%s' "$gate_out" \
            | sed -n 's/^gate: VERDICT=.* suite=\([^ ]*\).*$/\1/p' | tail -1)"
        [ -n "$gate_suite" ] || gate_suite=-

        if [ "$gate_rc" -ne 0 ]; then
            # A VERDICT THAT BLAMES NOBODY IS RECORDED ON THE BEAD ANYWAY. The bead is where
            # the next reader looks, and a NO_VERDICT that leaves no trace is how this morning
            # stayed invisible for fifty minutes: eleven consecutive withheld verdicts, each
            # one logged and none of them anywhere a person would see.
            log "CHECK6 $id: gate $gate_outcome on $br in $name (${gate_reason:-unspecified})"

            land_mark "$id" GATED "$tip" "$gate_outcome:${gate_reason:-unspecified}"

            # THE BASE'S OWN FAULT. Held, not reopened, not charged — and unlike a machinery
            # fault this one has a determinate owner, so it is filed against the repository
            # rather than escalated to the operator. The counter below is for a fault nobody
            # can be handed; a red base can be handed to whoever can change the code.
            #
            # ONCE PER REPOSITORY PER PASS. The intake dedupes on the ref, so a second call
            # would be correct and would still cost a database round trip and a recurrence
            # note for every held branch — five held branches would read as five recurrences
            # of a thing that happened once, and SIN_AT would escalate inside a single pass.
            if [ "$gate_rc" = "$SPIRA_GATE_BASEFAIL" ]; then
                # NOT `progress`. A held branch is not a movement of the DAG, and sending one
                # across the seam would mute the judgement tier's only check on paralysis —
                # which is precisely the condition a red base creates (see `act` above).
                log "CHECK6 $id: gate: held — the base fails its own gate; $name's gate is red against $base too (suite $gate_suite)"
                if _basefail_fix_check "$id" "$gate_out" "$gate_suite" "$name"; then
                    local _fse_cert="${_scan_extref[$id]:-}"
                    _fse_cert="${_fse_cert#basefail:$name:}"
                    if [ "${_scan_st[$id]:-}" = "closed" ]; then
                        log "CHECK6 $id: base-fix: $br is green on $name's red suite $_fse_cert — certifying"
                        land_mark "$id" CERTIFIED "$tip"
                        mark_submitted "$id" "$tip" certified
                        progress "certified $br in $name — base-fix (suite $_fse_cert)"
                    fi
                else
                    if [ "${basefail_filed:-}" != 1 ]; then
                        basefail_filed=1
                        base_incident "$name" "$gate_suite" "${gate_reason:-base-red}" \
                                      "$br" "$base" "$gate_out"
                    fi
                fi
                if _basefail_fix_check "$id" "$gate_out" "$gate_suite" "$name"; then
                    local _fse_cert="${_scan_extref[$id]:-}"
                    _fse_cert="${_fse_cert#basefail:$name:}"
                    _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
                    if [ "${_cur_st:-}" = "closed" ]; then
                        log "CHECK6 $id: base-fix: $br is green on $name's red suite $_fse_cert — certifying"
                        land_mark "$id" CERTIFIED "$tip"
                        mark_submitted "$id" "$tip" certified
                        progress "certified $br in $name — base-fix (suite $_fse_cert)"
                    fi
                fi
                continue
            fi

            if ! spira_gate_blames_branch "$gate_rc"; then
                # NO_VERDICT — the machinery could not judge, and no one can be handed that:
                # no reopen, no attempt, no note that reads as a rejection. The branch keeps
                # its turn and the next pass takes it.
                #
                # BUT A MACHINERY FAULT THAT REPEATS IS AN ESCALATION, not a retry forever.
                # Retrying forever is exactly what made today's livelock invisible — the pass
                # said "the next pass takes it" eleven times and was, each time, telling the
                # truth. The counter is per branch and per reason, so a lock that clears on
                # its own costs nothing and a lock that never clears reaches the operator.
                nv_key="$(printf '%s' "$br-${gate_reason:-unspecified}" | tr -c 'A-Za-z0-9._-' '-')"
                nv_file="$SPIRA_RUN/noverdict/$nv_key"
                mkdir -p "$SPIRA_RUN/noverdict"
                nv_n=$(( $(cat "$nv_file" 2>/dev/null || echo 0) + 1 ))
                printf '%s\n' "$nv_n" > "$nv_file"
                if [ "$nv_n" -ge "${SPIRA_NOVERDICT_MAX:-3}" ] && [ ! -e "$nv_file.asked" ]; then
                    : > "$nv_file.asked"
                    spira_ask_machinery "$id" "$br" "$name" "$gate_outcome" "$gate_reason" "$nv_n" "$gate_out"
                    progress "escalated $id — $gate_outcome x$nv_n on $br"
                fi
                continue
            fi

            # RE-READ THE BEAD. The gate took minutes; the bead may have been reopened
            # and claimed in that window. Reopening on a stale status puts already-open
            # work back on the board and charges an attempt it did not earn.
            _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
            if [ "${_cur_st:-}" != "closed" ]; then
                log "CHECK6 $id: bead is now ${_cur_st:--} (was closed at scan time) — not reopening $br"
                continue
            fi

            # THE BRANCH'S OWN FAULT — the only path that reopens and charges.
            local _rn_gate
            _rn_gate="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo '?')"
            bead_reopen "$id" gate-red "Reopened by sentinel: branch $br failed $name's landing gate. The branch carries $_rn_gate commit(s) from the previous session — the next aeon should resume from the existing work, not restart.

$(printf '%s' "$gate_out" | tail -20)"
            unset _rn_gate
            progress "reopened $id — failed the gate"
            spira_event bead.reopened "$id" "reopened $id — $br failed $name's landing gate" \
                "$(printf '%s' "$gate_out" | tail -3)" || true
            land_mark "$id" RED "$tip" gate
            unset 'judged[$br]'
            continue
        fi
        # A REUSED VERDICT IS SAID OUT LOUD, in the log an operator reads about the pass
        # rather than only in the gate's own meter. This leg is where the second full gate on
        # every bead used to be spent, so a pass that skipped one has done the thing this
        # cache was built for and should be legible as that — and a pass in which nothing is
        # ever reused is the first symptom of a key that has stopped matching anything, which
        # otherwise looks exactly like a busy queue.
        [ "${gate_reason:-}" = cached ] \
            && log "CHECK6 $id: gate PASS on $br in $name — this tree had already passed, so no suite ran"

        # A PASS CLEARS THE MACHINERY-FAULT COUNTERS FOR THIS BRANCH. Otherwise a branch that
        # queued behind a lock three times last week would escalate on its first hiccup this
        # week, and the escalation would be about nothing.
        rm -f "$SPIRA_RUN/noverdict/$(printf '%s' "$br" | tr -c 'A-Za-z0-9._-' '-')"* 2>/dev/null

        # RE-READ THE BEAD before landing. The scan is up to a pass old; the gate in
        # between takes minutes, and landing on a stale status is how landed work gets
        # reopened — the bead may have been reopened and claimed while the gate ran.
        _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
        if [ "${_cur_st:-}" != "closed" ]; then
            log "CHECK6 $id: bead is now ${_cur_st:--} (was closed at scan time) — not landing $br"
            continue
        fi

        case "$mode" in
        pr)
            if land_pr "$repo" "$br" "$id" "$base"; then
                mark_submitted "$id" "$tip" pr "$refresh"
                if [ "$refresh" -gt 0 ]; then
                    # A REFRESH IS NOT A MOVEMENT OF THE DAG, so it does not cross the seam.
                    # No bead changed state and nothing landed — the branch was only dragged
                    # back onto a base that moved. Reporting it as progress would count
                    # maintenance as throughput and mute CHECK 8, the one check that notices
                    # paralysis, for exactly as long as a branch went on failing to merge.
                    act "refreshed $br onto $base in $name — rebased, re-gated and force-pushed"
                else
                    land_mark "$id" REBASED "$tip" "pr-open:$name"
                    progress "opened a pull request for $br in $name"
                fi
            else
                mark_submitted "$id" "$tip" failed "$refresh"
            fi
            ;;
        hold)
            bdq note "$id" "Gated and held: $br passed $name's landing gate. Spira does not advance $name's $base_branch. Merge it by hand when you are ready — nothing else will." >/dev/null 2>&1
            mark_submitted "$id" "$tip" hold
            act "gated and held $br in $name — nothing here advances $base"
            ;;
        *)
            # A MERGE CONFLICT AND A REJECTED PUSH ARE NOT THE SAME FAILURE. The first is a
            # real disagreement the next aeon must resolve; the second only means the base
            # moved between our fetch and our push, and the fix is to fetch again and retry.
            # Conflating them reopened finished work that merged perfectly: law-cron pushes
            # the statute synthesis every four hours and export-beads the mirror every six,
            # so this raced roughly six times a day and each race cost a bead an attempt
            # toward poison.
            #
            # AND WHAT LANDS IS THE BRANCH'S OWN COMMITS, NEVER COPIES OF THEM. The retry
            # used to recover by rebasing the LANDING branch onto the moved base, which
            # replays the branch's commits as new objects and leaves the branch ref pointing
            # at the originals. The work landed; the branch was then, correctly, an ancestor
            # of nothing, so the reap kept it — `KEEP <id> unlanded — 2 commit(s) not in
            # origin/main` in the same pass that had just landed it — and the next pass
            # merged it again for a second `landed` line two minutes later. A no-op merge
            # still pushes: `git push` answers "Everything up-to-date" and exits 0, so the
            # duplicate reads as a movement and inflates the action count the judgement tier
            # reads. Rebasing the BRANCH moves its ref with its commits, so ancestry stays
            # the true test of "already landed" for everything downstream.
            #
            # The landing branch is therefore rebuilt from the base at the top of every
            # attempt rather than carried between them: once $br has been replayed, the
            # previous attempt's merge commit describes a base that no longer exists.
            #
            # A MISSING LANDING WORKTREE IS NOT A CONFLICT. Falling through to the merge with
            # no tree to merge in fails, and the failure arm reopens finished work with a
            # reason that is about the branch — a lie about a bead, and one that costs it an
            # attempt toward poison.
            if [ ! -e "$land/.git" ]; then
                log "CHECK6 $id: no landing worktree at $land — leaving $br to the next pass"
                continue
            fi
            merged=0; pushed=0; nothing=0; wedged=0; norebase=''; push_blocked=''; _merge_conflicts=''
            for attempt in 1 2 3; do
                # A landing worktree that will not check the base out is a broken worktree,
                # not a branch that conflicts — same reason as the guard above, and the same
                # cost if it is allowed to fall through to the merge.
                git -C "$land" checkout -q -B landing "$base" 2>/dev/null || { wedged=1; break; }
                _pre_merge="$(git -C "$land" rev-parse HEAD 2>/dev/null)"
                if ! git -C "$land" -c "user.name=${SPIRA_GIT_NAME:-spira}" -c "user.email=${SPIRA_GIT_EMAIL:-spira@spira.invalid}" merge --no-edit -q -m "spira: land $id" "$br" 2>/dev/null; then
                    _merge_conflicts="$(git -C "$land" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ')"
                    _merge_conflicts="${_merge_conflicts% }"
                    git -C "$land" merge --abort 2>/dev/null
                    merged=0; break
                fi
                # A MERGE THAT DOES NOT MOVE HEAD IS A NO-OP. This happens when the base
                # advanced between our content_landed check and this merge — a concurrent
                # fetch updated origin/main in the shared object store and the checkout
                # picked up the new base, which already contains the branch's content. The
                # push then says "Everything up-to-date" and exits 0, so merged=1 and
                # pushed=1 both land — and "landed" fires with no new commit on the base
                # (sp-tkrn, sp-x2yr). The --no-write-fetch-head flag on the fetches
                # narrows the race window by eliminating FETCH_HEAD lock contention; this
                # guard closes it by refusing to call a no-op push a landing.
                if [ "$(git -C "$land" rev-parse HEAD 2>/dev/null)" = "$_pre_merge" ]; then
                    merged=0; nothing=1; break
                fi
                merged=1
                # CAPTURE STDERR; do not assert a cause that was not checked. The push fails
                # for auth, network, hooks, and protected branches as well as for
                # non-fast-forward, and every failure used to reach the log as "the base
                # moved" — the one case this loop handles. Classify first; take the retry
                # path only when the evidence matches the diagnosis.
                _push_err="$(mktemp)"
                if git -C "$land" push -q "$base_remote" "landing:$base_branch" 2>"$_push_err"; then
                    pushed=1; rm -f "$_push_err"; break
                fi
                _push_msg="$(cat "$_push_err"; rm -f "$_push_err")"
                _push_first="$(printf '%s' "$_push_msg" | head -1)"
                case "$_push_msg" in
                    *non-fast-forward*|*"fetch first"*|*rejected*) ;;
                    *)
                        log "landing: push failed for $br: ${_push_first:-unknown}"
                        push_blocked="${_push_first:-unknown}"; break ;;
                esac
                # Looks like a non-fast-forward rejection — but a keyword is not proof of a
                # lost race (law-a-pattern-match-is-not-an-identity-check). Fetch, then verify
                # the base actually moved. If it did not, this is a different failure: the
                # push was blocked for a reason unrelated to our tip being behind, and
                # retrying the same push against the same base will fail the same way.
                _base_before="$(git -C "$repo" rev-parse "refs/remotes/$base_remote/$base_branch" 2>/dev/null || true)"
                # Rejected: someone else advanced the base between our fetch and our push.
                # Fetch it, replay the BRANCH onto it, and build the landing again from there.
                git -C "$repo" fetch -q --no-write-fetch-head "$base_remote" 2>/dev/null
                _base_after="$(git -C "$repo" rev-parse "refs/remotes/$base_remote/$base_branch" 2>/dev/null || true)"
                if [ "$_base_before" = "$_base_after" ]; then
                    log "landing: push failed for $br — rejected but $base did not move (${_push_first:-unknown})"
                    push_blocked="${_push_first:-unknown}"; break
                fi
                log "landing: push rejected, $base moved — retry $attempt"
                sleep "$attempt"
                # THE SAME RULE ON THE RETRY PATH. This arm falls through to "branch
                # conflicts with $base", so a ref reaped between the losing push and the
                # replay is reported as a disagreement that never happened — the identical
                # defect by the second of the two routes into a reopen.
                if ! rebase_branch "$br" "$base" "$repo" "$name"; then
                    [ "${REBASE_FAILURE:-}" = conflict ] || norebase="${REBASE_FAILURE:-unknown}"
                    merged=0; break
                fi
                # THE TIP IS RE-READ BECAUSE THE REBASE MOVED IT. `land_mark ... LANDED
                # "$tip"` is the memory every later reader trusts for "this commit is on the
                # base"; a tip from before the replay names a commit that is not, which is
                # the same false record by a shorter route.
                tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
                if content_landed "$repo" "$br" "$base"; then
                    # Whoever won the race carried this work with them. It is not a land and
                    # it is not a movement — but it is not silence either: an unlanded branch
                    # that stops here for a good reason has to say so, or it is
                    # indistinguishable from one nothing looked at
                    # (law-absence-needs-a-positive-control). The Sending reaps the ref.
                    merged=0; nothing=1; break
                fi
            done
            if [ "$wedged" = 1 ]; then
                log "CHECK6 $id: landing worktree at $land will not check out $base — leaving $br to the next pass"
            elif [ -n "$norebase" ]; then
                log "CHECK6 $id: the retry could not attempt a rebase of $br onto $base ($norebase) — not a conflict, leaving the bead closed"
            elif [ "$nothing" = 1 ]; then
                log "CHECK6 $id: $br adds nothing to $base once rebased — its work is already there, nothing to land"
            elif [ "$merged" = 1 ] && [ "$pushed" = 1 ]; then
                # The reap belongs to CHECK 6b, not here. This line used to be
                # `git branch -q -D "$br" 2>/dev/null`, which git REFUSES while the aeon's
                # worktree still holds the branch — so it never once succeeded, the branch
                # survived, and the next pass re-landed it and counted the action again:
                #   06:29:33 ACT landed spira/sp-stranded
                #   06:31:35 ACT landed spira/sp-stranded
                # `acted` was therefore never 0 and CHECK 8 could never fire, which is the
                # CHECK 2 false-action bug arriving by a second route.
                progress "landed $br"
                # RECORDED BEFORE ANYTHING ELSE THIS BRANCH DOES. Everything after this line
                # — the sweep, the reap — can fail or be interrupted, and the one fact that
                # must survive is that this commit is now on the base. Written after the push
                # rather than before, so a push that never landed can never leave a memory
                # saying it did (law-closed-is-not-landed, one layer in).
                land_mark "$id" LANDED "$tip" "$name"
                rm -f "$LANDSTATE/$id.ejected" 2>/dev/null || true
                gh_issue_closeout "$id" \
                    "$(git -C "$land" rev-parse HEAD 2>/dev/null)" "$repo" || true
                # AFTER the push, never before it: the event says the commit is on the base
                # branch, which is the one claim CLOSED does not make (law-closed-is-not-landed).
                spira_event bead.landed "$id" "landed $br on $name's $base" \
                    "merged as $(git -C "$land" rev-parse --short HEAD 2>/dev/null) from $tip" || true
                # THE BASE HAS MOVED, SO EVERY SURVIVOR IS NOW BEHIND IT. Last, because
                # everything above is about the branch that just landed and must not be
                # delayed by other branches' rebases; and unset first, so this branch is not
                # replayed onto a base that already contains it.
                unset 'judged[$br]'
                [ "${#judged[@]}" -gt 0 ] \
                    && rebase_survivors "$repo" "$name" "$base" "$br" "${!judged[@]}"
            elif [ "$merged" = 1 ]; then
                # Merged fine, could not push. Nothing is wrong with the work; leave the bead
                # closed and let the next pass land it. Two distinct reasons reach here:
                # (a) push_blocked — a non-race push failure (auth, hook, network) already
                #     logged with its actual cause; the message here is for the operator's
                #     summary view, not a repeat of the detail.
                # (b) three genuine race retries exhausted — the race-retry message below.
                git -C "$land" reset -q --hard "$base" 2>/dev/null
                if [ -n "$push_blocked" ]; then
                    log "landing: $br merges clean but push failed — leaving closed"
                else
                    log "landing: $br merges clean but push kept losing the race — retrying next pass"
                fi
            else
                git -C "$land" merge --abort 2>/dev/null
                # ALREADY LANDED? ASK THE COMMIT GRAPH BEFORE REOPENING.
                #
                # A branch whose commits are all in the base cannot be merged again, and the
                # failure looks exactly like a first-time conflict from here. On 2026-09-12
                # sp-ce9 landed as 31fcf0d at 05:36:36 and was reopened at 05:38:32 for
                # "conflicts with origin/main" — it conflicted BECAUSE it was already in
                # origin/main. Left open it would have been re-claimed and the finished work
                # redone, which during a single-epic focus period spends the capacity that
                # period exists to protect.
                #
                # law-closed-is-not-landed cuts both ways: a REOPEN is also a claim about the
                # commit graph, so it must be checked against the graph rather than against a
                # branch's mergeability. Ancestry, never a tip comparison — a tip moves under
                # you mid-pass.
                # TWO TESTS, because ancestry ALONE IS NOT ENOUGH. sp-ce9 was caught by
                # ancestry; sp-lzt was not, and was reopened anyway at 06:11:18 on
                # 2026-09-12 after the sending leg had touched the branch between the
                # landing and the re-merge. Ancestry asks "is this TIP in the base", which
                # a moved tip answers no to even when every change it carried is in.
                #
                # The second test is the one the reopen message itself already computes:
                # commits on the branch not in the base. Zero means the branch introduces
                # nothing — there is nothing to conflict about and nothing to redo.
                # FETCH BEFORE ASSERTING A NEGATIVE. Both tests read $base, a
                # remote-tracking ref, and a stale one makes a landed branch look unlanded.
                # Measured 2026-09-12 06:17:40: sp-733 was judged "ancestor=no,
                # commits-ahead=?" and reopened, while the identical commands run by hand
                # minutes later returned ancestor=yes and 0 — the branch had landed as
                # b85e5db and $repo's origin/main had not caught up. This is the rule that
                # already existed and that the first two versions of this guard did not
                # apply: re-fetch immediately before asserting that something did NOT land.
                git -C "$repo" fetch -q --no-write-fetch-head origin 2>/dev/null || true
                local _rn_merge _anc=no
                _rn_merge="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo '?')"
                git -C "$repo" merge-base --is-ancestor "$br" "$base" 2>/dev/null && _anc=yes
                # A '?' means the count could not be TAKEN, which is not evidence of a
                # conflict. Refusing to reopen on an unreadable signal is the safe
                # direction: a bead left closed that should be open is visible as missing
                # work, while a bead reopened on finished work silently redoes it.
                if [ "$_anc" = yes ] || [ "$_rn_merge" = 0 ] || [ "$_rn_merge" = '?' ]; then
                    log "landing: $br introduces nothing new to $base (ancestor=$_anc, commits-ahead=$_rn_merge) — landed, not conflicted; not reopening $id"
                    spira_event bead.landed "$id" "landed $br on $name's $base" \
                        "branch introduces no commit $base lacks; a conflict here means already-merged" || true
                    unset 'judged[$br]' _rn_merge _anc
                    continue
                fi
                # DIAGNOSABLE WHEN IT FIRES ANYWAY. Both numbers are recorded, so a future
                # spurious reopen is a fact to read rather than a sequence to reconstruct
                # from timestamps across two logs.
                log "landing: $br genuinely conflicts with $base (ancestor=$_anc, commits-ahead=$_rn_merge)"
                local _merge_other _merge_note
                _merge_other="$(other_beads_on_conflicts "$repo" "$br" "$base" "${_merge_conflicts:-${REBASE_CONFLICTS:-}}")"
                if [ -n "$_merge_other" ]; then
                    _merge_note="Reopened by sentinel: branch $br conflicts with $base. The branch carries $_rn_merge commit(s) from the previous session. Those files were changed on $base by $_merge_other — check whether this work is already landed before resolving."
                else
                    _merge_note="Reopened by sentinel: branch $br conflicts with $base. The branch carries $_rn_merge commit(s) from the previous session — rebase onto $base, resolve the conflict, and finish. A merge conflict is not an escalation."
                fi
                bead_reopen "$id" rebase-conflict "$_merge_note"
                unset _rn_merge _anc _merge_other _merge_note _merge_conflicts
                # Counter labels (sp-requeue-N) no longer written (sp-lzt).
                progress "reopened $id — branch conflicts with $base"
                spira_event bead.reopened "$id" "reopened $id — $br conflicts with $name's $base" \
                    "the merge would not apply; rebase and finish" || true
                unset 'judged[$br]'
            fi
            ;;
        esac
    done
    if [ "$_budget_cut" = 1 ]; then
        _n_unvisited=0
        _past_cut=0
        for _ubr in $brs; do
            [ "$_past_cut" = 1 ] && _n_unvisited=$(( _n_unvisited + 1 ))
            [ "$_ubr" = "$br" ] && _past_cut=1
        done
        log "landing: budget cut at $br — $(( LAND_MAXSEC - ($(date +%s) - PASS_START) ))s left, $(( _n_unvisited + 1 )) branch(es) deferred in $name"
    fi

    # ==========================================================================
    # PHASE 2: PARALLEL CERTIFICATION. Branches deferred to _cert_brs above are
    # dispatched to gate.sh up to certify_pass_par at a time. Each result is
    # processed the moment its gate finishes (wait -n -p), not in dispatch order,
    # so a fast gate is never blocked behind a slow one. All DB writes stay on
    # one process.
    # ==========================================================================
    if [ "${#_cert_brs[@]}" -gt 0 ]; then
        local -a _cp_pids=() _cp_brs=() _cp_ids=() _cp_tips=() _cp_tmps=()
        log "landing: certify phase 2: ${#_cert_brs[@]} candidate(s) in $name (before tier sort)"

        # Waits for whichever running gate finishes next, then applies cert logic.
        # Uses wait -n -p (bash 5.1+) to collect results in completion order so a
        # fast gate is never blocked behind a slower one dispatched before it.
        # name/repo/base/basefail_filed are read via dynamic scope.
        _cert_process_result() {
            local br id tip gate_rc gate_out gate_outcome gate_reason gate_suite
            local _rn_cert nv_key nv_file nv_n _cur_st
            local _finished_pid _idx _tmp
            wait -n -p _finished_pid "${_cp_pids[@]}" 2>/dev/null; gate_rc=$?
            _idx=0
            while [ "$_idx" -lt "${#_cp_pids[@]}" ] && \
                  [ "${_cp_pids[$_idx]}" != "$_finished_pid" ]; do
                _idx=$(( _idx + 1 ))
            done
            _tmp="${_cp_tmps[$_idx]}"
            br="${_cp_brs[$_idx]}"; id="${_cp_ids[$_idx]}"; tip="${_cp_tips[$_idx]}"
            _cp_pids=("${_cp_pids[@]:0:$_idx}" "${_cp_pids[@]:$((_idx+1))}")
            _cp_brs=("${_cp_brs[@]:0:$_idx}" "${_cp_brs[@]:$((_idx+1))}")
            _cp_ids=("${_cp_ids[@]:0:$_idx}" "${_cp_ids[@]:$((_idx+1))}")
            _cp_tips=("${_cp_tips[@]:0:$_idx}" "${_cp_tips[@]:$((_idx+1))}")
            _cp_tmps=("${_cp_tmps[@]:0:$_idx}" "${_cp_tmps[@]:$((_idx+1))}")
            gate_out="$(cat "$_tmp" 2>/dev/null)"; rm -f "$_tmp"
            gate_outcome="$(spira_gate_outcome "$gate_rc")"
            gate_reason="$(printf '%s' "$gate_out" \
                | sed -n 's/^gate: VERDICT=[A-Z_]* reason=\([^ ]*\).*$/\1/p' | tail -1)"
            gate_suite="$(printf '%s' "$gate_out" \
                | sed -n 's/^gate: VERDICT=.* suite=\([^ ]*\).*$/\1/p' | tail -1)"
            [ -n "$gate_suite" ] || gate_suite=-
            if [ "$gate_rc" -ne 0 ]; then
                log "CHECK6 $id: certification gate $gate_outcome on $br in $name (${gate_reason:-unspecified})"
                land_mark "$id" GATED "$tip" "$gate_outcome:${gate_reason:-unspecified}"
                if [ "$gate_rc" = "$SPIRA_GATE_BASEFAIL" ]; then
                    log "CHECK6 $id: held — the base fails its own gate (suite $gate_suite)"
                    if _basefail_fix_check "$id" "$gate_out" "$gate_suite" "$name"; then
                        local _fse_cert="${_scan_extref[$id]:-}"
                        _fse_cert="${_fse_cert#basefail:$name:}"
                        if [ "${_scan_st[$id]:-}" = "closed" ]; then
                            log "CHECK6 $id: base-fix: $br is green on $name's red suite $_fse_cert — certifying"
                            land_mark "$id" CERTIFIED "$tip"
                            mark_submitted "$id" "$tip" certified
                            progress "certified $br in $name — base-fix (suite $_fse_cert)"
                        fi
                    else
                        if [ "${basefail_filed:-}" != 1 ]; then
                            basefail_filed=1
                            base_incident "$name" "$gate_suite" "${gate_reason:-base-red}" \
                                          "$br" "$base" "$gate_out"
                        fi
                    fi
                    if _basefail_fix_check "$id" "$gate_out" "$gate_suite" "$name"; then
                        local _fse_cert="${_scan_extref[$id]:-}"
                        _fse_cert="${_fse_cert#basefail:$name:}"
                        _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
                        if [ "${_cur_st:-}" = "closed" ]; then
                            log "CHECK6 $id: base-fix: $br is green on $name's red suite $_fse_cert — certifying"
                            land_mark "$id" CERTIFIED "$tip"
                            mark_submitted "$id" "$tip" certified
                            progress "certified $br in $name — base-fix (suite $_fse_cert)"
                        fi
                    fi
                    return 0
                fi
                if ! spira_gate_blames_branch "$gate_rc"; then
                    nv_key="$(printf '%s' "$br-${gate_reason:-unspecified}" | tr -c 'A-Za-z0-9._-' '-')"
                    nv_file="$SPIRA_RUN/noverdict/$nv_key"
                    mkdir -p "$SPIRA_RUN/noverdict"
                    nv_n=$(( $(cat "$nv_file" 2>/dev/null || echo 0) + 1 ))
                    printf '%s\n' "$nv_n" > "$nv_file"
                    if [ "$nv_n" -ge "${SPIRA_NOVERDICT_MAX:-3}" ] && [ ! -e "$nv_file.asked" ]; then
                        : > "$nv_file.asked"
                        spira_ask_machinery "$id" "$br" "$name" "$gate_outcome" "$gate_reason" "$nv_n" "$gate_out"
                        progress "escalated $id — $gate_outcome x$nv_n on $br"
                    fi
                    [ "${gate_reason:-}" = timeout ] && land_mark "$id" RED "$tip" timeout
                    return 0
                fi
                _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
                if [ "${_cur_st:-}" != "closed" ]; then
                    log "CHECK6 $id: bead is now ${_cur_st:--} (was closed at scan time) — not reopening $br"
                    return 0
                fi
                _rn_cert="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo '?')"
                bead_reopen "$id" cert-gate-red "Reopened by sentinel: branch $br failed $name's certification gate. The branch carries $_rn_cert commit(s) from the previous session — the next aeon should resume from the existing work, not restart.

$(printf '%s' "$gate_out" | tail -20)"
                progress "reopened $id — failed the certification gate"
                spira_event bead.reopened "$id" "reopened $id — $br failed $name's certification gate" \
                    "$(printf '%s' "$gate_out" | tail -3)" || true
                land_mark "$id" RED "$tip" gate
                return 0
            fi
            [ "${gate_reason:-}" = cached ] \
                && log "CHECK6 $id: certification PASS on $br in $name — this tree had already passed"
            rm -f "$SPIRA_RUN/noverdict/$(printf '%s' "$br" | tr -c 'A-Za-z0-9._-' '-')"* 2>/dev/null
            _cur_st="$(bdjson show "$id" 2>/dev/null | python3 -c '
import sys,json
try:d=json.load(sys.stdin)
except:raise SystemExit
d=d if isinstance(d,list) else [d]
print(d[0].get("status","-") if d else "-")' 2>/dev/null)"
            if [ "${_cur_st:-}" != "closed" ]; then
                log "CHECK6 $id: bead is now ${_cur_st:--} (was closed at scan time) — not certifying $br"
                return 0
            fi
            land_mark "$id" CERTIFIED "$tip"
            mark_submitted "$id" "$tip" certified
            progress "certified $br in $name — queued"
        }

        # PHASE 2 TIER ORDERING. Never-gated branches (no prior landstate record) go first;
        # branches with a non-RED landstate go second; RED branches with a moved tip or a
        # base-red reason go third. A RED branch whose tip is unchanged and whose failure is
        # not base-red is skipped — the same tip will produce the same result, and the slot
        # is better spent on a branch that has never been tried. Timeouts write RED (above)
        # and are subject to the same skip condition.
        local -a _t0_brs=() _t0_ids=() _t0_tips=()
        local -a _t1_brs=() _t1_ids=() _t1_tips=()
        local -a _t2_brs=() _t2_ids=() _t2_tips=()
        local _p2_skip=0 _p2_ls _p2_ls_st _p2_ls_tip _p2_ls_reason _p2_ci _p2_br _p2_id _p2_tip
        for _p2_ci in "${!_cert_brs[@]}"; do
            _p2_br="${_cert_brs[$_p2_ci]}"
            _p2_id="${_cert_beadids[$_p2_ci]}"
            _p2_tip="${_cert_tips[$_p2_ci]}"
            _p2_ls_st=""; _p2_ls_tip=""; _p2_ls_reason=""
            _p2_ls="$(land_state "$_p2_id" 2>/dev/null || true)"
            read -r _p2_ls_st _p2_ls_tip _ _p2_ls_reason <<< "$_p2_ls"
            if [ -z "${_p2_ls_st:-}" ]; then
                _t0_brs+=("$_p2_br"); _t0_ids+=("$_p2_id"); _t0_tips+=("$_p2_tip")
            elif [ "${_p2_ls_st:-}" = RED ] && [ "${_p2_ls_tip:-}" = "$_p2_tip" ] && \
                 [ "${_p2_ls_reason:-}" != "base-red" ]; then
                _p2_skip=$(( _p2_skip + 1 ))
                log "CHECK6 $_p2_id: tip unchanged since RED mark (reason=${_p2_ls_reason:-unknown}) — skipping re-gate of $_p2_br"
            elif [ "${_p2_ls_st:-}" = RED ]; then
                _t2_brs+=("$_p2_br"); _t2_ids+=("$_p2_id"); _t2_tips+=("$_p2_tip")
            else
                _t1_brs+=("$_p2_br"); _t1_ids+=("$_p2_id"); _t1_tips+=("$_p2_tip")
            fi
        done
        log "landing: certify phase 2 tiers in $name: never-gated=${#_t0_brs[@]} non-red=${#_t1_brs[@]} red=${#_t2_brs[@]} skipped=$_p2_skip width=${certify_pass_par}"
        _cert_brs=("${_t0_brs[@]+"${_t0_brs[@]}"}" "${_t1_brs[@]+"${_t1_brs[@]}"}" "${_t2_brs[@]+"${_t2_brs[@]}"}")
        _cert_beadids=("${_t0_ids[@]+"${_t0_ids[@]}"}" "${_t1_ids[@]+"${_t1_ids[@]}"}" "${_t2_ids[@]+"${_t2_ids[@]}"}")
        _cert_tips=("${_t0_tips[@]+"${_t0_tips[@]}"}" "${_t1_tips[@]+"${_t1_tips[@]}"}" "${_t2_tips[@]+"${_t2_tips[@]}"}")

        local _ci _ctmp _dbr _did _dtip
        for _ci in "${!_cert_brs[@]}"; do
            _dbr="${_cert_brs[$_ci]}"; _did="${_cert_beadids[$_ci]}"; _dtip="${_cert_tips[$_ci]}"
            if ! gate_fits; then
                case "${_scan_extref[${_dbr#spira/}]:-}" in
                    basefail:"$name":*)
                        log "CHECK6 $_did: base-fix branch — gating despite budget exhaustion" ;;
                    *)
                        _budget_cut=1; break ;;
                esac
            fi
            while [ "${#_cp_pids[@]}" -ge "${certify_pass_par:-1}" ]; do
                _cert_process_result
            done
            _ctmp="$(mktemp -t spira-cert.XXXXXX)"
            gate_lock_wait
            SPIRA_GATE_LOCK_WAIT="$_gate_wait" SPIRA_GATE_BEAD="$_did" \
                SPIRA_GATE_SUITES="${SPIRA_CERTIFY_SUITES:-on}" \
                "$SPIRA_HOME/gate.sh" "$_dbr" "$name" >"$_ctmp" 2>&1 &
            _cp_pids+=("$!"); _cp_brs+=("$_dbr"); _cp_ids+=("$_did")
            _cp_tips+=("$_dtip"); _cp_tmps+=("$_ctmp")
        done
        while [ "${#_cp_pids[@]}" -gt 0 ]; do
            _cert_process_result
        done
        _land_state "repo=$name"
    fi

    return 0
}

# SOURCED, THIS MUST NOT RUN A PASS. Reading a function out of this file — content_landed is
# the one worth borrowing — otherwise executes a full landing over every repository as a side
# effect of the `.`, which is how a diagnostic became a live pass over 29 branches while its
# author was asking a read-only question.

# EARLY QUEUE CHECK: run before certification gates so a batch whose CI finished before the
# pass started lands in seconds rather than waiting for the gate sequence (up to 90 min).
# Idempotent with the late check: a batch that lands here is gone by the late check, and one
# still pending here is the one the late check settles.
for repo_name in $(spira_repos); do
    [ "$(repo_land "$repo_name")" = queue ] || continue
    bash "$SPIRA_HOME/queue.sh" step "$repo_name" 2>&1 \
        | while IFS= read -r _bl; do log "queue early: $_bl"; done || true
done

# VERDICT FIRST (hotfix, concierge 2026-09-21, sp-len2q): the queue step (land a green
# batch, eject a red one, open the next) ran only after every repository's certification
# walk, so a PR that went green at 03:08Z waited behind an hour of gating. Landing green
# work is the highest-value action in the pass; take it before certifying anything. The
# step runs again below so newly certified branches can still form a batch this pass.
for repo_name in $(spira_repos); do
    [ "$(repo_land "$repo_name")" = queue ] || continue
    bash "$SPIRA_HOME/queue.sh" step "$repo_name" 2>&1 \
        | while IFS= read -r _bl; do log "$_bl"; done || true
done
for repo_name in $(spira_repos); do
    land_repo "$repo_name"
done

# After all certification passes, settle each queue-mode repo's open batch and open the next.
for repo_name in $(spira_repos); do
    [ "$(repo_land "$repo_name")" = queue ] || continue
    bash "$SPIRA_HOME/queue.sh" step "$repo_name" 2>&1 \
        | while IFS= read -r _bl; do log "queue late: $_bl"; done || true
done

# ======================================================================================
# ADVANCE THE CHECKOUT HUMANS READ — UNCONDITIONALLY, not only when a branch merged this
# pass. Landing pushes the base branch from a worktree and nothing else pulls the home
# checkout, so the shared checkout stays behind until something advances it. That used to
# happen inside the branch loop, which means it only ran when a branch landed. A base ref
# that moved by any other route — a push from another box, a PR merged on GitHub, a hand-
# landing — was never picked up; skew.sh noticed an hour later and escalated rather than
# repairing. Now it is one pass behind at most.
#
# PUSH AND QUEUE BOTH ADVANCE THE BASE THROUGH SPIRA. pr leaves it to GitHub; hold leaves it
# to a human. Only those two have a checkout to advance.
# ======================================================================================
for repo_name in $(spira_repos); do
    _rfsh_repo="$(repo_root "$repo_name" 2>/dev/null)" || continue
    [ -e "$_rfsh_repo/.git" ] || continue
    _rfsh_mode="$(repo_land "$repo_name" 2>/dev/null)"
    [ "$_rfsh_mode" = push ] || [ "$_rfsh_mode" = queue ] || continue
    _rfsh_out="$("$SPIRA_HOME/skew.sh" refresh "$_rfsh_repo" 2>&1)" || true
    [ -n "${_rfsh_out:-}" ] && log "$_rfsh_out"
done

# The sweep's counters are appended only when it did something. A clause that reads
# "0 rebased, 0 conflicted" on every quiet pass is noise, and this line is the one a reader
# greps to see whether a pass moved anything at all.
_gh_unlanded_scan || true

sweep_note=""
[ $(( n_swept + n_swept_conflict )) -gt 0 ] \
    && sweep_note=", $n_swept survivor(s) rebased after a landing, $n_swept_conflict conflicted"

# UNIT INSTALLATION ENSURE. After every pass, install any unit templates that landed
# since the last install.sh run. Nothing else re-renders after a landing; this closes
# that gap without the full install.sh (no live-aeons fence, no re-enabling everything).
_land_ue="$SPIRA_REPO/systemd/unit-ensure.sh"
[ -x "$_land_ue" ] && bash "$_land_ue" 2>&1 | while IFS= read -r _ue_l; do log "$_ue_l"; done || true
unset _land_ue _ue_l

# CARGO BUILD ENSURE. Fires build.sh when cargo source changed or a binary is absent
# while its unit is enabled. Cargo-absent boxes skip silently. Extraction into
# land-build-ensure.sh makes the trigger testable without a full landing pass.
_land_be="$SPIRA_REPO/spira/land-build-ensure.sh"
[ -x "$_land_be" ] && bash "$_land_be" 2>&1 | while IFS= read -r _be_l; do log "$_be_l"; done || true
unset _land_be _be_l

log "landing: pass complete — $n_branches branch(es) seen, $n_prog movement(s)$sweep_note"
