#!/usr/bin/env bash
#
# landing.sh — CHECK 6, moved out of the reconcile loop and into its own process.
#
#   landing.sh          one landing pass over every repository
#
# WHY THIS IS NOT IN THE SENTINEL ANY MORE
# ----------------------------------------
# The sentinel is a reconcile loop and every other check in it is a handful of bead queries
# and a `git for-each-ref`. Landing is not: it fetches, rebases, runs the repository's whole
# landing gate and pushes. Measured, an ordinary pass took 21s and the one pass
# that landed a branch took 5m30s — and because CHECK 7 (summon an aeon for ready work) sat
# BELOW it, a free aeon slot with 15 beads ready stayed empty for those five and a half
# minutes. systemd will not start a second instance of a oneshot that is already running, so
# the long pass also swallowed the three timer ticks behind it.
#
# That inverts the tenet the harness is built on: cheap deterministic work must not queue
# behind expensive work. Summoning costs about a second and is pure dispatch; gating runs a
# repository's whole test suite. Reordering the two checks was the one-line version and it
# was rejected on purpose — it recovers the free slot but a long landing still swallows the
# ticks behind it, so the loop's period would still be set by its most expensive step.
#
# THE UNIT NAME IS THE MUTEX. The sentinel launches this as a transient unit with a FIXED
# name (`spira-landing`), fire and forget, the same shape lib.sh already uses to summon an
# aeon. systemd refuses to start a unit that is already active, so a pass arriving while a
# landing is still in flight declines and moves on — the wanted behaviour, and it costs no
# lockfile and no pid file to get. `--collect` is load-bearing rather than tidiness: without
# it a FAILED unit stays loaded and every later `systemd-run --unit=spira-landing` would be
# refused forever, which is a landing leg that stops dead and says nothing.
#
# FIRE AND FORGET NEEDS A POSITIVE CONTROL, because "nothing landed" and "the landing worker
# never ran" look identical from the sentinel's pane (law-absence-needs-a-positive-control).
# So this writes two things the sentinel reads on its next pass:
#
#   landing.status    — key=value, rewritten whole every run: when it finished, its exit
#                       status, how many spira/* branches it actually SAW. A run that saw
#                       branches and landed none is a different fact from a run that saw
#                       none, and a leg that has not completed a run in half an hour is a
#                       third.
#   landing.progress  — an append-only mailbox, one line per movement of the DAG. The
#                       sentinel drains it (by rename, so a line is counted once and only
#                       once) and replays each line through its own `progress`, which is
#                       what keeps landings visible in the sentinel log and counted against
#                       the judgement tier. Landing does not get to count its own actions
#                       into a pass it is no longer part of.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

STATUS="$SPIRA_RUN/landing.status"
MAILBOX="$SPIRA_RUN/landing.progress"

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

# gate_lock_wait -> how long this pass may wait for a repository's gate tree, in seconds.
#
# THE RESERVE IS FOR RUNNING THE GATE, NOT FOR QUEUING TO START IT. gate_fits has just
# guaranteed LAND_GATE_RESERVE seconds remain; spending them waiting on a lock would burn a
# whole pass to land nothing, so the wait gets a slice and the run keeps the rest. A pass with
# no limit at all still does not wait forever here — an operator draining a backlog wants the
# branches whose trees are free, not a pass parked on the first one that is not.
gate_lock_wait() {
    local slice=$(( LAND_GATE_RESERVE / 10 ))
    [ "$slice" -lt 30 ] && slice=30
    echo "$slice"
}

log "landing: starting a pass over [$(spira_repos | tr '\n' ' ')]"

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
LANDSTATE="$SPIRA_RUN/landstate"
land_state() {           # land_state <id> -> "<state> <tip> <at>" or empty
    local f="$LANDSTATE/$1"
    [ -r "$f" ] || return 1
    tr -d '\n' < "$f" 2>/dev/null
}
land_mark() {            # land_mark <id> <state> <tip> [reason]
    mkdir -p "$LANDSTATE" 2>/dev/null || return 0
    printf '%s %s %s %s' "$2" "${3:-none}" "$(date +%s)" "${4:-}" > "$LANDSTATE/$1.$$" 2>/dev/null \
        && mv -f "$LANDSTATE/$1.$$" "$LANDSTATE/$1" 2>/dev/null
}
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
            local _other_beads _reopen_note _rq_n _rn_sweep
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
                bead_reopen "$id" "$_reopen_note"
                progress "reopened $id — does not rebase onto $base"
                spira_event bead.reopened "$id" "reopened $id — $br does not rebase onto $base in $name" \
                    "conflicts in ${REBASE_CONFLICTS:-unknown}; the next aeon is handed the rebase" || true
            fi
            land_mark "$id" RED "$(git -C "$repo" rev-parse "$br" 2>/dev/null)" no-rebase
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
    local norebase was _ref _obj gate_suite basefail_filed= _cur_st
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
    local -A _scan_st=() _scan_repo=() _scan_labels=() _scan_superseded=()
    local _scan_ids=""
    for br in $brs; do
        _scan_ids="$_scan_ids ${br#spira/}"
    done
    if [ -n "${_scan_ids// /}" ]; then
        local _sid _sst _srepo _ssup _slabels
        # shellcheck disable=SC2086
        while IFS=$'\t' read -r _sid _sst _srepo _ssup _slabels; do
            [ -n "${_sid:-}" ] || continue
            _scan_st["$_sid"]="$_sst"
            _scan_repo["$_sid"]="$_srepo"
            _scan_labels["$_sid"]="$_slabels"
            _scan_superseded["$_sid"]="${_ssup:-0}"
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
    # sup BEFORE labels: bash whitespace-IFS collapses consecutive tabs, so an empty labels
    # field followed by a non-empty sup field would produce the wrong token order. With sup
    # first, only the trailing labels tab can be empty, and trailing whitespace IFS is stripped.
    print(f"{bid}\t{st}\t{repo}\t{sup}\t{labels}")
' "$(spira_home_repo)" 2>/dev/null)
    fi

    for br in $brs; do
        id="${br#spira/}"
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
            local _other_beads _reopen_note _rq_n _rn_land
            _rn_land="$(git -C "$repo" rev-list --count "$base..$br" 2>/dev/null || echo '?')"
            _other_beads="$(other_beads_on_conflicts "$repo" "$br" "$base" "${REBASE_CONFLICTS:-}")"
            _reopen_note="Reopened by sentinel: $br does not rebase onto $base in $name; conflicts in ${REBASE_CONFLICTS:-unknown}. The branch carries $_rn_land commit(s) from the previous session — resume from the existing work."
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
                bead_reopen "$id" "$_reopen_note"
                progress "reopened $id — does not rebase onto $base"
                spira_event bead.reopened "$id" "reopened $id — $br does not rebase onto $base in $name" \
                    "conflicts in ${REBASE_CONFLICTS:-unknown}; the next aeon is handed the rebase" || true
            fi
            land_mark "$id" RED "$tip" no-rebase
            continue
        fi
        tip="$(git -C "$repo" rev-parse "$br" 2>/dev/null)"
        judged["$br"]=1

        # CONFINEMENT COMES BEFORE THE GATE. A spike's branch may pass every test in the
        # repository and still be the wrong thing to merge — its experiment compiles, which
        # is the point of an experiment. This asks a different question from the gate ("is
        # this branch allowed to land at all") and it must be asked first, because the gate
        # is the expensive half and there is nothing to learn from running it on a branch
        # that is going back either way. A bead that is not a spike passes through untouched.
        if ! gate_out="$("$SPIRA_HOME/confine.sh" "$id" "$br" "$repo" "$base" "${bead_labels:-}" 2>&1)"; then
            bead_reopen "$id" "Reopened by sentinel: $gate_out"
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
            log "landing: $(( LAND_MAXSEC - ($(date +%s) - PASS_START) ))s left in this pass — not starting $name's gate for $id; the next pass takes it"
            return 0
        fi
        # THE GATE'S TREE IS SHARED WITH EVERY OTHER GATE OF THIS REPOSITORY, so it may be
        # busy, and a pass on a clock must not sit in that queue: the wait is capped at what
        # this pass can spare rather than the gate's own default, which is longer than a whole
        # pass. Waiting it out would land nothing and be killed mid-gate for the privilege.
        # THE BEAD IS NAMED TO THE GATE, because this pass is the only caller that knows it
        # for certain. The gate's yield record otherwise derives the bead from the branch
        # name, which is right only while a branch is named after the bead it was cut for —
        # and a branch's affinity is recorded precisely because that is not always true
        # (law-branch-affinity-is-recorded).
        gate_out="$(SPIRA_GATE_LOCK_WAIT="$(gate_lock_wait)" SPIRA_GATE_BEAD="$id" \
            "$SPIRA_HOME/gate.sh" "$br" "$name" 2>&1)"
        gate_rc=$?
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
                if [ "${basefail_filed:-}" != 1 ]; then
                    basefail_filed=1
                    base_incident "$name" "$gate_suite" "${gate_reason:-base-red}" \
                                  "$br" "$base" "$gate_out"
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
            bead_reopen "$id" "Reopened by sentinel: branch $br failed $name's landing gate. The branch carries $_rn_gate commit(s) from the previous session — the next aeon should resume from the existing work, not restart.

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
                    land_mark "$id" REBASED "$tip" pr-open
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
            merged=0; pushed=0; nothing=0; wedged=0; norebase=''
            for attempt in 1 2 3; do
                # A landing worktree that will not check the base out is a broken worktree,
                # not a branch that conflicts — same reason as the guard above, and the same
                # cost if it is allowed to fall through to the merge.
                git -C "$land" checkout -q -B landing "$base" 2>/dev/null || { wedged=1; break; }
                _pre_merge="$(git -C "$land" rev-parse HEAD 2>/dev/null)"
                if ! git -C "$land" merge --no-edit -q -m "spira: land $id" "$br" 2>/dev/null; then
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
                if git -C "$land" push -q "$base_remote" "landing:$base_branch" 2>/dev/null; then pushed=1; break; fi
                # Rejected: someone else advanced the base between our fetch and our push.
                # Fetch it, replay the BRANCH onto it, and build the landing again from there.
                git -C "$repo" fetch -q --no-write-fetch-head "$base_remote" 2>/dev/null
                log "landing: push rejected, $base moved — retry $attempt"
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
                # Merged fine, could not push after three rebases. Nothing is wrong with the
                # work; leave the bead closed and let the next pass land it.
                git -C "$land" reset -q --hard "$base" 2>/dev/null
                log "landing: $br merges clean but push kept losing the race — retrying next pass"
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
                bead_reopen "$id" "Reopened by sentinel: branch $br conflicts with $base. The branch carries $_rn_merge commit(s) from the previous session — rebase onto $base, resolve the conflict, and finish. A merge conflict is not an escalation."
                unset _rn_merge _anc
                # Counter labels (sp-requeue-N) no longer written (sp-lzt).
                progress "reopened $id — branch conflicts with $base"
                spira_event bead.reopened "$id" "reopened $id — $br conflicts with $name's $base" \
                    "the merge would not apply; rebase and finish" || true
                unset 'judged[$br]'
            fi
            ;;
        esac
    done
    return 0
}

# SOURCED, THIS MUST NOT RUN A PASS. Reading a function out of this file — content_landed is
# the one worth borrowing — otherwise executes a full landing over every repository as a side
# effect of the `.`, which is how a diagnostic became a live pass over 29 branches while its
# author was asking a read-only question.
for repo_name in $(spira_repos); do
    land_repo "$repo_name"
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
# ONLY PUSH-MODE REPOS HAVE A CHECKOUT TO ADVANCE. A pr-mode repository's checkout is not
# where work lands; GitHub advances its base when a PR merges.
# ======================================================================================
for repo_name in $(spira_repos); do
    _rfsh_repo="$(repo_root "$repo_name" 2>/dev/null)" || continue
    [ -e "$_rfsh_repo/.git" ] || continue
    [ "$(repo_land "$repo_name" 2>/dev/null)" = push ] || continue
    _rfsh_out="$("$SPIRA_HOME/skew.sh" refresh "$_rfsh_repo" 2>&1)" || true
    [ -n "${_rfsh_out:-}" ] && log "$_rfsh_out"
done

# The sweep's counters are appended only when it did something. A clause that reads
# "0 rebased, 0 conflicted" on every quiet pass is noise, and this line is the one a reader
# greps to see whether a pass moved anything at all.
sweep_note=""
[ $(( n_swept + n_swept_conflict )) -gt 0 ] \
    && sweep_note=", $n_swept survivor(s) rebased after a landing, $n_swept_conflict conflicted"
log "landing: pass complete — $n_branches branch(es) seen, $n_prog movement(s)$sweep_note"
