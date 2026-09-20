#!/usr/bin/env bash
#
# archivist.sh — rescue a full session's unfinished business before it is thrown away.
#
#   archivist.sh sweep            every live session; archive the ones that have drifted
#   archivist.sh now [<session>]  archive one session now, whatever its context (the manual
#                                 path — "hibernate", before a deliberate clear)
#   archivist.sh list             what the sweep can see, and what it would do about each
#   archivist.sh mark <s> <state> [<n>]   the running archivist's own progress writes
#   archivist.sh state [<s>]      print what the status line and the dashboard are reading
#   archivist.sh digest <t> [<from-turn>] [--full]
#                                 the transcript rendered small enough for an agent to read
#
# WHAT PROBLEM THIS IS. Context is re-read in full on every turn, so a long session costs many
# times a fresh one for identical work, and the fix — clearing — is exactly what nobody dares
# do, because a session near the ceiling is also the session carrying the most that was never
# written down: questions asked and never answered, findings stated and never filed, verdicts
# acted on and never recorded. So the expensive state is also the sticky one. This makes
# clearing cheap by making the loss impossible.
#
# IT READS THE TRANSCRIPT, NOT THE CONVERSATION. Everything it needs is already on disk: every
# turn, every tool result, every verdict. So it costs the session it is rescuing exactly
# nothing — no turn, no tokens, no interruption. That is not an optimisation, it is the design
# constraint: a persistence step that itself adds turns makes the problem it exists to solve
# slightly worse every time it runs, and would be worst in the sessions that need it most.
#
# WHY THE ARCHIVIST IS NOT A FAYTH, and this was the open question when this was designed. An
# aeon's subject is a BEAD: it claims one under a lease, cuts a branch, commits, and is judged
# by whether a commit names the bead. The archivist's subject is a TRANSCRIPT. Giving it a bead
# per session would mean the machinery for rescuing unfinished business itself manufactured one
# unfinished bead per session — and none of the apparatus buys anything here, because it writes
# beads and wiki notes rather than code, so there is no branch, no landing gate and nothing to
# rebase. The watcher therefore invokes it directly, and the state file below is what stands in
# for a lease: it is the record that a sweep happened, how far it got, and when.
#
# WHERE THE WATCHER LIVES, and this was the other open question: its own timer, not a sentinel
# check. The sentinel returns the moment the bead graph is healthy — which is exactly when a
# session at the keyboard is most likely to be quietly filling up — so a check hung off the end
# of a pass would miss the common case entirely. Its subject is orthogonal to the bead graph,
# and its cadence is different: a lease dies in minutes, a session crosses a band over tens of
# them.
#
# CONCURRENCY IS CAPPED AT ONE ARCHIVE AT A TIME, across both entry points, by a single
# archivist-wide lock. Type=oneshot plus the timer's refusal to restart an active unit means
# one sweep cannot overlap the next, but a manual `now <session>` is outside both mechanisms
# by construction — the per-session lock only prevents two callers archiving the SAME session.
# The archivist-wide lock is what prevents two callers archiving DIFFERENT sessions at once:
# the sweep skips a session it cannot lock and moves on; the manual path waits, because an
# operator told "busy, try later" will simply run it again in a loop.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

ARC="$SPIRA_RUN/archivist"
PROMPT_FILE="$SPIRA_CHAMBER/archivist.md"
ARC_LOCK="$ARC/archivist.lock"

# HOW MANY BANDS A SESSION HAS ALREADY CROSSED, from the threshold it has NOT yet reached.
# `SP_CTX_NEXT` is the next one above the current context, so "next is high" means warn is
# behind it. `over` means past every one of them. Still used for the band-3 push notification.
crossed() { case "$1" in warn) echo 0 ;; high) echo 1 ;; limit) echo 2 ;; over) echo 3 ;; *) echo -1 ;; esac; }

# ---- the state file, which is a contract with two readers ------------------------------
# ctx-meter.sh renders it in the status line and cockpit/health.sh renders it on the
# dashboard, both from $SPIRA_RUN/archivist/<session>.state as flat key=value lines:
#
#   state=sweeping|archiving|safe|failed|capacity
#   at_turn=<the session's turn count when this state was computed>
#   items_filed=<how many beads and notes were written>
#
# AT_TURN IS LOAD-BEARING, NOT BOOKKEEPING. "Safe to clear" is a statement about the session as
# it was when the sweep read it; forty turns later it describes a session that no longer exists,
# and acting on it discards everything said since. Both readers demote a stale verdict to "safe
# as of N turns ago" on exactly this number. A state written without it would have the meter
# telling the operator it is safe to discard work the archivist never saw.
#
# AND IT IS WRITTEN AS THE WORK HAPPENS, never only at the end. A long sweep that shows nothing
# is indistinguishable from an archivist that never ran, which is the state the operator is
# trying to escape.
# The exit code `archive` uses to say "the account refused this, and will refuse the next one
# too". Out of the way of a real failure's own code, and named so the two call sites cannot
# drift from the site that raises it.
ARC_RC_CAPACITY=77

write_state() {          # write_state <session> <state> <at_turn> <items>
    mkdir -p "$ARC" 2>/dev/null || return 1
    local tmp="$ARC/.$1.$$"
    printf 'state=%s\nat_turn=%s\nitems_filed=%s\n' "$2" "$3" "$4" > "$tmp" || return 1
    mv -f "$tmp" "$ARC/$1.state"
}
state_key() {            # state_key <session> <key>
    sed -n "s/^$2=//p" "$ARC/$1.state" 2>/dev/null | head -1
}

# ---- the high-water mark, for the band-3 notification only ------------------------------
# The band-3 push fires once per session: when the context reaches the limit band AND the
# archivist found something to file. The hwm prevents re-notifying after a second sweep of the
# same session pushes it past the threshold again. The .notified sentinel is the primary guard;
# the hwm is kept so the notification block can read it without re-deriving the band.
set_hwm() {              # set_hwm <session> <band-rank>
    mkdir -p "$ARC" 2>/dev/null || return 1
    local tmp="$ARC/.$1.hwm.$$"
    printf 'band=%s\n' "$2" > "$tmp" && mv -f "$tmp" "$ARC/$1.hwm"
}
get_hwm() { sed -n 's/^band=//p' "$ARC/$1.hwm" 2>/dev/null | head -1; }

# ---- how far the transcript has already been read ---------------------------------------
# A session that crosses `high` and later `limit` is archived twice, over a transcript whose
# first half is the same both times. Without a covered mark the second pass re-reads what the
# first already filed and files it again — and the duplicate is not merely noise: two asks
# carrying one question is the shape that once had a single reply close both, recording a
# verdict against a question nobody answered.
#
# IT IS WRITTEN ONLY AFTER A RUN SUCCEEDS. A failed pass has covered nothing, whatever it read.
covered() { sed -n 's/^turn=//p' "$ARC/$1.covered" 2>/dev/null | head -1; }
set_covered() {          # set_covered <session> <turn>
    local sid="$1" turn="$2"
    # REFUSE a non-numeric turn. Writing "-" or "?" as a cursor is what turned one bad meter
    # read into a 4.5-hour archivist outage: every subsequent sweep dies on that cursor, killing
    # the entire pass, not just the affected session (sp-ci5cn).
    if ! [[ "$turn" =~ ^[0-9]+$ ]]; then
        log "archivist: REFUSED set_covered $sid — turn='${turn:-(empty)}' is not numeric; cursor unchanged"
        return 1
    fi
    local tmp="$ARC/.$sid.covered.$$"
    printf 'turn=%s\n' "$turn" > "$tmp" && mv -f "$tmp" "$ARC/$sid.covered"
}

# Coerce a meter or cursor value to a non-negative integer. The established "unreadable"
# sentinel in this codebase is "-" or "?"; ${x:-0} does not replace either because both are
# non-empty strings, so they pass silently into arithmetic and produce a syntax error. Pass
# every meter value and cursor through this before arithmetic. Returns 0 always — the log line
# is the signal, not the exit code; the caller must not fail just because the meter was blind.
arc_numeric() {   # arc_numeric <value> <context-for-log> → echoes the number
    local v="${1:-}" ctx="${2:-value}"
    if [[ "$v" =~ ^[0-9]+$ ]]; then echo "$v"; return 0; fi
    # STDERR: this function is always called via $( ), so stdout is captured as the return value.
    # log() writes to stdout; redirecting to stderr keeps the log line out of the captured output.
    log "archivist: $ctx is '${v:-(empty)}' — treating as 0 (not covered)" >&2
    echo "0"
}

# ---- which transcripts are a live session at the keyboard -------------------------------
# THREE FILTERS, AND EACH ONE EXISTS BECAUSE ITS ABSENCE PRODUCES A WRONG ANSWER RATHER THAN A
# NOISY ONE.
#
#   too old       Everything this rescues is rescued so the session can be cleared, which only
#                 matters while somebody is still in it. A transcript untouched for longer than
#                 SPIRA_ARCHIVIST_IDLE is history, and sweeping the whole disk on every pass is
#                 the unbounded fan-out this kind of box has a load-274 scar from
#                 (law-fence-loops-on-shared-hardware).
#
#   the harness's own   An aeon's unfinished business is its BEAD — that is the entire point of
#                 a lease and a reaper — so archiving an aeon's transcript files a second,
#                 worse copy of work the graph already tracks. The archivist's own session is
#                 excluded by the same rule, and must be: it runs with its working directory
#                 inside $SPIRA_RUN precisely so that it is, and without that the sweep would
#                 eventually archive the archivist.
#
#   empty         A session that has not spoken has nothing to rescue.
#
# The exclusion is derived from $SPIRA_RUN rather than written out, because the client's
# project directory is the working directory with every non-alphanumeric character replaced by
# a hyphen — so any path under the runtime directory has that directory's slug as a prefix. A
# literal here would be one operator's layout, and would stop matching the day they moved it.
live_transcripts() {
    python3 - "$SPIRA_TOKEN_PROJECTS" "$SPIRA_RUN" "$SPIRA_ARCHIVIST_IDLE" <<'PY'
import glob, os, re, sys, time
projects, run, idle = sys.argv[1], sys.argv[2], int(sys.argv[3])
def slug(p): return re.sub(r"[^A-Za-z0-9]", "-", p)
# BOTH THE PATH AS CONFIGURED AND THE PATH RESOLVED. The client derives a project directory
# from the working directory it was GIVEN, so a runtime directory reached through a symlink
# produces a slug of the literal path — while an aeon started from the resolved one produces
# a slug of that. Matching only one of them lets the other through, and what comes through is
# the harness's own sessions being archived as if they were the operator's.
ours = {slug(run), slug(os.path.realpath(run))}
now = time.time()
for f in sorted(glob.glob(os.path.join(projects, "*", "*.jsonl"))):
    d = os.path.basename(os.path.dirname(f))
    if any(d.startswith(o) for o in ours):
        continue
    try:
        st = os.stat(f)
    except OSError:
        continue
    if st.st_size == 0 or now - st.st_mtime > idle:
        continue
    # The client names a transcript for the session it holds, so the basename IS the session
    # id — the same key both readers of the state file compose their path from.
    print("%s\t%s" % (os.path.basename(f)[:-6], f))
PY
}

# ---- one archive run --------------------------------------------------------------------
# The whole input is the transcript path and the numbers that say why it is being read. No
# bead, no branch, no worktree — see the header.
#
# THE PROMPT IS A FILE IN THE CHAMBER, beside the personas, because it is the same kind of
# artifact: what an agent is told it is for. It is not a `.fayth`, and that is deliberate —
# a fayth declares a partition of the bead graph and this persona has none.
# ---- what came before this transcript ---------------------------------------------------
# CLEARING STARTS A NEW TRANSCRIPT WITH A NEW SESSION ID, so "the session log" is only ever the
# tail of the conversation. The client records a stable lineage id across those clears and
# archive.sh indexes it, which makes the chain a query instead of a guess — and it is the same
# query the manual salvage path runs, so the two cannot come to different views of what one
# conversation is.
#
# IT IS A HINT, NEVER A DEPENDENCY. The archive is swept on its own timer, so a session cleared
# minutes ago may not be indexed yet, and a colleague may have no archive at all. Every failure
# here is the same answer — a chain of one — because the current transcript is the only one this
# sweep is actually acting on either way.
lineage_brief() {        # lineage_brief <session> <this transcript>
    local rows
    [ -x "$HERE/archive.sh" ] || { printf 'unknown — no transcript archive on this installation.'; return; }
    rows="$("$HERE/archive.sh" lineage "$1" --json 2>/dev/null)" || rows=""
    # THE ROWS GO IN ON STDIN, not interpolated into the script. They carry paths an operator
    # typed, and a value pasted into a program is a value that can end the string it is in.
    rows="$(printf '%s' "$rows" | python3 -c '
import json, sys
me = sys.argv[1]
for ln in sys.stdin:
    try: r = json.loads(ln)
    except Exception: continue
    if r.get("source") and r["source"] != me:
        print("    %s  (%s turns, up to %s)" % (r["source"], r.get("turns"), r.get("last_ts")))
' "$2")"
    if [ -n "$rows" ]; then
        printf 'This session is a continuation. Earlier transcripts in the same conversation,\noldest first:\n\n%s\n\nThey were swept when they were live, so read one only to resolve something the\ncurrent transcript refers to and does not contain.' "$rows"
    else
        printf 'A chain of one — nothing earlier belongs to this conversation, or the archive has\nnot indexed it yet.'
    fi
}

archive() {              # archive <session> <transcript> <at_turn> <ctx> <why> [wait]
    local sid="$1" tp="$2" at="$3" ctx="$4" why="$5" lock_mode="${6:-try}" from
    from="$(arc_numeric "$(covered "$sid")" "session $sid prior cursor")"
    [ -f "$PROMPT_FILE" ] || { log "archivist: no prompt at $PROMPT_FILE"; return 1; }
    mkdir -p "$ARC/cwd" || return 1

    # A SUBSHELL WITH TWO LOCKS ON FILE DESCRIPTORS, so both are released when the shell exits
    # however it exits — a lock cleared by a trap is a lock leaked whenever the process is
    # killed, and a stale one would wedge every later sweep of that session silently.
    (
        # THE ARCHIVIST-WIDE LOCK (fd 8). One archive at a time across both entry points. The
        # sweep passes lock_mode=try and skips if busy; the manual path passes lock_mode=wait,
        # because the operator asked and "busy, try later" invites a retry loop.
        if [ "$lock_mode" = wait ]; then
            log "archivist: $sid — waiting for the archivist-wide lock"
            flock 8 || { log "archivist: $sid could not take the archivist-wide lock"; exit 75; }
        else
            flock -n 8 || { log "archivist: $sid skipped — another archive is already running"; exit 75; }
        fi

        # THE PER-SESSION LOCK (fd 9). Prevents two callers archiving the SAME session.
        # NON-ZERO, so the caller reads a refused lock as "this pass did not archive it" and
        # goes no further.
        flock -n 9 || { log "archivist: $sid is already being archived"; exit 75; }

        write_state "$sid" sweeping "$at" 0
        local prompt logf rc items
        # `${x//y/z}` and not sed: the transcript path and the database path are paths an
        # operator typed, and a `|` or a `&` in one is a sed expression rather than a value.
        prompt="$(cat "$PROMPT_FILE")"
        prompt="${prompt//\{\{TRANSCRIPT\}\}/$tp}"
        prompt="${prompt//\{\{SESSION\}\}/$sid}"
        prompt="${prompt//\{\{DB\}\}/$SPIRA_DB}"
        prompt="${prompt//\{\{CTX\}\}/$ctx}"
        prompt="${prompt//\{\{TURNS\}\}/$at}"
        prompt="${prompt//\{\{FROM_TURN\}\}/$from}"
        prompt="${prompt//\{\{WHY\}\}/$why}"
        prompt="${prompt//\{\{LINEAGE\}\}/$(lineage_brief "$sid" "$tp")}"
        prompt="${prompt//\{\{ARCHIVIST\}\}/$HERE/archivist.sh}"
        prompt="${prompt//\{\{NOTIFY\}\}/$SPIRA_HOME/mail.sh}"
        # EMPTY IS "YOU HAVE NO WIKI", NEVER A GUESS, and it substitutes a whole paragraph
        # rather than a path — a brief that rendered as a bare empty string would leave the
        # agent with a sentence pointing at nowhere, which is worse than no sentence. A
        # colleague cloning this has no wiki at all, and a path invented for them is a
        # directory an agent would create and fill.
        if [ -n "$SPIRA_WIKI" ]; then
            prompt="${prompt//\{\{WIKI\}\}/Craft knowledge — how a tool really behaves, which approach failed and why, the
shape of a recurring hazard — goes on a page under \`$SPIRA_WIKI\`, not into a bead.
Append to the page it belongs on; create one only if none fits.}"
        else
            prompt="${prompt//\{\{WIKI\}\}/There is no wiki configured on this installation, so craft knowledge has nowhere
better to go than an insight. Record it as one and say in the body that it wants a
home.}"
        fi

        logf="$SPIRA_RUN/archivist-$sid.log"
        log "archivist: $sid — $why (ctx $ctx, turn $at) -> $logf"
        # STREAMED, like an aeon's, so the log is a live trace rather than a buffered dump: with
        # the default format nothing reaches it until the session ends, so it cannot answer "is
        # this working" during the only window in which that question is asked.
        #
        # THE BINARY IS INJECTABLE for the same reason it is in aeon.sh, and a PATH shim cannot
        # substitute: conf.sh replaces $PATH outright, so a suite that put a fake `claude` first
        # on PATH would run the real model against the operator's account, silently and at cost.
        #
        # NO Edit IN THE TOOL LIST. The archivist writes beads, notes and pages; a tool for
        # changing a line in a file that already exists is the one it would need in order to
        # start finishing the work it finds, which is the one thing it must not do.
        #
        # AND IT RUNS IN $ARC/cwd. The client derives a transcript's project directory from the
        # working directory, so this is what keeps the archivist's own transcript inside
        # $SPIRA_RUN and therefore outside its own sweep.
        cd "$ARC/cwd" || exit 1
        # --add-dir loads brain's CLAUDE.md (page schema: frontmatter, filenames, log.md, index.md).
        # $ARC/cwd has no CLAUDE.md, so this is the only project instructions source.
        printf '%s' "$prompt" | timeout "$SPIRA_ARCHIVIST_TIMEOUT" \
            "${SPIRA_AGENT:-claude}" -p --output-format stream-json --verbose \
                   --model "$SPIRA_ARCHIVIST_MODEL" \
                   --allowedTools "Bash,Read,Grep,Glob,Write" \
                   --dangerously-skip-permissions \
                   ${SPIRA_WIKI:+--add-dir "$SPIRA_WIKI"} \
            > "$logf" 2>&1
        rc=$?

        # THE COUNT COMES FROM THE STATE FILE, which the run itself has been updating as it
        # filed each item — not from parsing the session's output. A number scraped out of a
        # model's prose is a number the model chose to print, and one that was silently absent
        # would render as "0 filed" beside a green "safe to clear": the reading that says the
        # sweep found nothing worth keeping, which is the one claim that must never be a
        # parsing failure in disguise.
        items="$(state_key "$sid" items_filed)"; items="${items:-0}"
        # AT_TURN STAYS AT THE TURN THE SWEEP READ UP TO, deliberately, and is not refreshed to
        # the turn it finished at. The session may have kept talking while this ran, and those
        # turns are exactly the ones nobody persisted — so the verdict must describe the
        # session the archivist actually saw, and be demoted by both readers if it has moved on.
        if [ "$rc" -eq 0 ]; then
            write_state "$sid" safe "$at" "$items"
            set_covered "$sid" "$at"
            log "archivist: $sid safe to clear — $items item(s) filed"
        elif reset_at="$(capacity_reset_at "$logf")"; then
            # REFUSED IS NOT FAILED, and the difference is whether this session is ever looked
            # at again. `failed` is excluded from the sweep by design (see prev_state below),
            # because a run that broke will break identically on the next pass and re-trigger
            # every two minutes. An account refusal is the opposite: nothing about this session
            # caused it and the next pass after the window reopens would succeed — so recording
            # it as `failed` retires the session permanently for a condition that heals itself,
            # and the dashboard goes on saying "! archive failed" long after capacity returned.
            #
            # The sweep-level guard above cannot catch this one. It reads the pause BEFORE the
            # pass starts, and a window can shut mid-run: on 2026-09-10 this archivist launched
            # at 16:38:33, was refused at 16:38:34, and the pause file it would have honoured
            # was not written until 16:39:01 — 27 seconds too late to have been seen.
            #
            # THE PAUSE IS SET FROM HERE for the same reason aeon.sh sets it: this is the first
            # part of the harness to learn the window is shut, and every summon between now and
            # the next refusal is one the account will reject.
            capacity_pause_set "$reset_at" "archivist/$sid"
            write_state "$sid" capacity "$at" "$items"
            log "archivist: $sid deferred — the account refused the session, retrying when the window reopens at $(date -d "@$reset_at" +%H:%M 2>/dev/null)"
            # A DISTINCT CODE, because the caller must do a third thing with this. Zero would
            # count a session that was never read as archived; a plain failure would send the
            # pass on to the next session, and every one of those launches is a `claude` the
            # account has already said it will refuse.
            rc=$ARC_RC_CAPACITY
        else
            write_state "$sid" failed "$at" "$items"
            log "archivist: $sid FAILED rc=$rc after $items item(s) — see $logf"
        fi
        exit "$rc"
    ) 8>"$ARC_LOCK" 9>"$ARC/$sid.lock"
}

# ---- the modes ---------------------------------------------------------------------------
case "${1:-sweep}" in

sweep|list)
    MODE="${1:-sweep}"

    # HONOUR THE PAUSE. The archivist spends from the same five-hour window the aeons draw on,
    # and capacity_paused is the one predicate that says it is shut. Checked once per pass, not
    # per session: the window does not reopen mid-pass, and a per-session check invites four
    # identical log lines.
    if [ "$MODE" = sweep ] && capacity_paused; then
        log "archivist: skipped — account capacity paused for another ${SPIRA_CAPACITY_LEFT}s"
        # WRITE A SWEEP-LEVEL STATE so the cockpit can distinguish "skipped for capacity" from
        # "idle" and "failed". The file is named for the sweep, not a session: it is cleared by
        # the next pass that runs normally.
        mkdir -p "$ARC" 2>/dev/null
        printf 'sweep_state=skipped\nreason=capacity\nat=%s\n' "$(date +%s)" > "$ARC/sweep.state"
        exit 0
    fi

    # ---- measure every live session -------------------------------------------------------
    # Collected into parallel arrays so they can be sorted by drift before the budget is spent.
    declare -a C_SID=() C_TP=() C_CTX=() C_TURNS=() C_NXT=() C_BAND=() C_DRIFT=() C_WOULD=() C_PREV=()
    [ "$MODE" = list ] && printf '%-40s %10s %6s %8s %5s %s\n' SESSION CONTEXT TURNS BAND DRIFT WOULD
    while IFS=$'\t' read -r sid tp; do
        [ -n "$sid" ] || continue
        # ONE MEASUREMENT, SHARED. ctx-meter.sh is what the status line and the dashboard read,
        # so asking it — rather than parsing the transcript again here — is what stops the
        # watcher from acting on a number the operator is not being shown.
        e="$("$HERE/ctx-meter.sh" env "$tp" 2>/dev/null)" || e=""
        ctx="$(sed -n 's/^SP_CTX_NOW=//p' <<<"$e")"
        turns="$(sed -n 's/^SP_CTX_TURNS=//p' <<<"$e")"
        nxt="$(sed -n 's/^SP_CTX_NEXT=//p' <<<"$e")"
        band="$(crossed "${nxt:-}")"
        [ "$band" -lt 0 ] 2>/dev/null && continue          # the meter could not read it

        # THE TRIGGER IS TURNS SINCE THE LAST SWEEP, not the context band. What the archivist
        # covers is turns — covered() is a turn cursor, the prompt takes {{FROM_TURN}}, and the
        # cost of a run is proportional to the turns since the last one. Context depth tells you
        # the session is expensive; turns since the last sweep tells you work is uncovered.
        #
        # arc_numeric GUARDS BOTH INPUTS. ctx-meter.sh emits "-" when a field could not be read
        # (SP_CTX_TURNS_LEFT=- appears in the same trace that produced the outage). ${x:-0} does
        # not replace "-" — it is not empty — so the raw value would reach the arithmetic and
        # crash the entire sweep, stopping all sessions, not just the one with the bad cursor.
        cov="$(arc_numeric "$(covered "$sid")" "session $sid cursor")"
        turns_n="$(arc_numeric "${turns:-}" "session $sid turns")"
        drift=$(( turns_n - cov ))

        # A SESSION WHOSE LAST RUN FAILED IS NOT RE-FIRED. covered is written only on success,
        # so without this gate a failure would re-trigger on every pass — drift stays above the
        # threshold because covered was never advanced. The failure is reported through the state
        # file, where the operator is already looking; leave it to them.
        prev_state="$(state_key "$sid" state)"

        would=hold
        if [ "$drift" -ge "$SPIRA_ARCHIVIST_EVERY" ] && [ "$prev_state" != "failed" ]; then
            would=archive
        fi
        if [ "$MODE" = list ]; then
            printf '%-40s %10s %6s %8s %5s %s\n' "$sid" "${ctx:--}" "${turns:--}" "$band" "$drift" "$would"
        fi
        C_SID+=("$sid"); C_TP+=("$tp"); C_CTX+=("${ctx:-0}"); C_TURNS+=("$turns_n")
        C_NXT+=("${nxt:-}"); C_BAND+=("$band"); C_DRIFT+=("$drift"); C_WOULD+=("$would")
        C_PREV+=("$prev_state")
    done < <(live_transcripts)
    [ "$MODE" = list ] && exit 0

    # SORT CANDIDATES BY DRIFT DESCENDING, so the budget is spent on the most drifted session
    # first. With a budget of 1 the choice of WHICH session matters: ordering by discovery
    # would let a chatty session at the front of the list starve the one carrying the most
    # unpersisted work, indefinitely.
    sorted_idx="$(python3 -c '
import sys
drifts = [int(x) for x in sys.argv[1].split(",") if x]
for i in sorted(range(len(drifts)), key=lambda i: drifts[i], reverse=True):
    print(i)' "$(IFS=,; echo "${C_DRIFT[*]+"${C_DRIFT[*]}"}")" 2>/dev/null)"

    archived=0
    while IFS= read -r idx; do
        [ -n "$idx" ] || continue
        sid="${C_SID[$idx]}"; tp="${C_TP[$idx]}"; ctx="${C_CTX[$idx]}"; turns="${C_TURNS[$idx]}"
        band="${C_BAND[$idx]}"; drift="${C_DRIFT[$idx]}"; would="${C_WOULD[$idx]}"

        [ "$would" = archive ] || continue

        # BUDGET THE PASS. At most SPIRA_ARCHIVIST_PER_PASS sessions per sweep; the rest are
        # left for the next tick five minutes later. Serial-and-unbounded is what turns a quiet
        # morning into a 20-minute pass.
        if [ "$archived" -ge "$SPIRA_ARCHIVIST_PER_PASS" ]; then
            log "archivist: $sid deferred — budget of $SPIRA_ARCHIVIST_PER_PASS reached (drift $drift)"
            continue
        fi
        archive "$sid" "$tp" "$turns" "$ctx" "turns since last sweep ($drift) >= $SPIRA_ARCHIVIST_EVERY"
        arc_rc=$?
        # THE WINDOW IS SHUT FOR THE WHOLE PASS, not for this session. Carrying on would launch
        # one `claude` per remaining session for the account to refuse in turn, and each would
        # write its own deferred state for the same single cause. The pause this raised means
        # the next pass stops at the sweep-level guard instead of reaching here at all.
        if [ "$arc_rc" -eq "$ARC_RC_CAPACITY" ]; then
            log "archivist: sweep stopped — the account's window is shut; the rest of this pass is deferred"
            break
        fi
        [ "$arc_rc" -eq 0 ] || continue
        archived=$((archived + 1))

        # Record the band so a later sweep can read it without re-deriving. This is only for
        # the notification below; the archive trigger is turns, not bands.
        set_hwm "$sid" "$band"

        # THE ONE PUSH, AND ONLY FROM THE TOP BAND. Everything below this is already delivered
        # by the two readers of the state file — the status line and the dashboard both render
        # "safe to clear (n filed)" the moment it is written, at no cost and with nothing to
        # dismiss. Sending a message on every archive as well would put a notice in front of
        # the operator on every long session, and a standing list that never changes becomes
        # wallpaper — which is how a real one comes to land in a pane they have learned to
        # ignore (law-alerts-must-be-actionable).
        #
        # Past the LIMIT is different: the session is at the ceiling, every further turn is
        # charged at the full context, and they may not be looking at either pane. So: once per
        # session, never repeated, and only when there was something to say.
        [ "$band" -ge 3 ] || continue
        [ -e "$ARC/$sid.notified" ] && continue
        filed="$(state_key "$sid" items_filed)"; filed="${filed:-0}"
        [ "$filed" -gt 0 ] 2>/dev/null || continue
        : > "$ARC/$sid.notified"
        local _arc_subj="A session at the keyboard is carrying $ctx tokens; its unfinished business is now saved ($filed item(s)) and it is safe to clear"
        [ -x "$SPIRA_HOME/mail.sh" ] && "$SPIRA_HOME/mail.sh" send operator \
            --from "Archivist <archivist@spira>" \
            --subject "$_arc_subj" \
            --kind note <<MAILEOF >/dev/null 2>&1
## Note
$_arc_subj

Every further turn re-reads all of it. The archivist swept it at turn $turns; anything said since is not covered.
MAILEOF
    done <<< "$sorted_idx"

    # CLEAR THE SWEEP STATE on a normal pass, so a stale "skipped" does not linger.
    rm -f "$ARC/sweep.state"

    # ---- what the sessions that no longer exist left behind ---------------------------
    # A state, a mark and a log per session, forever, in a directory nothing else prunes.
    # Tied to the TRANSCRIPT rather than to an age: while the client still holds the
    # transcript the state is still the truth about it, and once the transcript is gone
    # there is no session for any of it to describe.
    for f in "$ARC"/*.state; do
        [ -e "$f" ] || break
        s="$(basename "$f")"; s="${s%.state}"
        [ "$s" = sweep ] && continue     # the sweep-level state is not a session
        [ -z "$(find "$SPIRA_TOKEN_PROJECTS" -name "$s.jsonl" -print -quit 2>/dev/null)" ] || continue
        rm -f "$ARC/$s.state" "$ARC/$s.hwm" "$ARC/$s.covered" "$ARC/$s.notified" \
              "$ARC/$s.lock" "$SPIRA_RUN/archivist-$s.log"
        log "archivist: forgot $s — its transcript is gone"
    done
    ;;

now)
    # THE MANUAL PATH — hibernate. Same machinery, different trigger: no threshold, no
    # high-water mark, because the operator asking is the trigger and asking twice is a
    # deliberate act. With no argument it takes the session most recently written to, which is
    # the one they are sitting in.
    sid="${2:-}"; tp=""
    if [ -n "$sid" ]; then
        case "$sid" in
            */*) tp="$sid"; sid="$(basename "$tp")"; sid="${sid%.jsonl}" ;;
            *)   tp="$(live_transcripts | awk -F'\t' -v s="$sid" '$1==s{print $2; exit}')" ;;
        esac
    else
        read -r sid tp < <(python3 - "$SPIRA_TOKEN_PROJECTS" <<'PY'
import glob, os, sys
c = glob.glob(os.path.join(sys.argv[1], "*", "*.jsonl"))
c = [f for f in c if os.path.getsize(f) > 0]
if c:
    f = max(c, key=os.path.getmtime)
    print("%s %s" % (os.path.basename(f)[:-6], f))
PY
)
    fi
    [ -n "$tp" ] && [ -f "$tp" ] || die "no transcript for '${2:-the newest session}'"
    e="$("$HERE/ctx-meter.sh" env "$tp" 2>/dev/null)" || e=""
    ctx="$(sed -n 's/^SP_CTX_NOW=//p' <<<"$e")"
    turns="$(sed -n 's/^SP_CTX_TURNS=//p' <<<"$e")"
    archive "$sid" "$tp" \
        "$(arc_numeric "${turns:-}" "session $sid turns")" \
        "$(arc_numeric "${ctx:-}" "session $sid ctx")" \
        "asked for by hand" wait
    ;;

mark)
    # THE RUNNING ARCHIVIST'S OWN PROGRESS WRITES. It calls this as it works — `archiving` the
    # moment it files the first item, then again with a running count — so the pane moves while
    # the sweep is happening rather than jumping from nothing to a verdict.
    sid="${2:?mark needs a session}"; st="${3:?mark needs a state}"
    case "$st" in
        sweeping|archiving|safe|failed|capacity) ;;
        *) die "unknown archivist state '$st'" ;;
    esac
    # AT_TURN IS NEVER INVENTED HERE. There is no state file to take it from unless a run is in
    # progress, and a fabricated 0 would make every later verdict read as freshly computed at
    # turn zero — which both readers would render as a very stale "safe", or worse, as a fresh
    # one on a session with no turns.
    at="$(state_key "$sid" at_turn)"
    [ -n "$at" ] || die "no archivist run in progress for $sid — nothing to mark"
    items="${4:-$(state_key "$sid" items_filed)}"
    write_state "$sid" "$st" "$at" "${items:-0}"
    ;;

digest)
    # THE TRANSCRIPT, RENDERED SMALL ENOUGH TO READ. This is shipped rather than left to the
    # archivist to improvise because the cost of getting it wrong is the whole feature: a long
    # session's transcript is megabytes of JSON, and an agent that reads it whole spends more
    # context rescuing the session than the session was carrying. Deterministic, so two runs
    # over one transcript see the same thing, and testable, which a prompt is not.
    #
    # TOOL RESULTS ARE DROPPED AND TOOL CALLS ARE KEPT. The results are the bulk of the bytes by
    # an order of magnitude and are the one part already summarised in the prose beside them —
    # whereas the CALLS are the record of work in flight, which is half of what is being hunted:
    # which branches were touched, which beads were claimed, what was half-done. `--full` keeps
    # a truncated head of each result for a session whose findings really are in its output.
    tp="${2:?digest needs a transcript}"; from="${3:-0}"; full=0
    case "${4:-}" in --full) full=1 ;; esac
    python3 - "$tp" "$from" "$full" <<'PY'
import json, sys
tp, frm, full = sys.argv[1], int(sys.argv[2]), sys.argv[3] == "1"

def text(c):
    """The human-readable parts of a content field, which may be a string or a block list."""
    if isinstance(c, str): return [("text", c)]
    out = []
    for b in c if isinstance(c, list) else []:
        if not isinstance(b, dict): continue
        t = b.get("type")
        if t == "text" and b.get("text", "").strip():
            out.append(("text", b["text"]))
        elif t == "tool_use":
            inp = b.get("input") or {}
            # ONE LINE PER CALL, and the line is whichever field says what it acted ON. A tool
            # name alone ("Edit") records that something was edited and loses the only part
            # that matters, which is what.
            arg = ""
            for k in ("command", "file_path", "pattern", "path", "url", "prompt", "description"):
                if isinstance(inp.get(k), str) and inp[k].strip():
                    arg = " ".join(inp[k].split())[:160]; break
            out.append(("tool", "%s %s" % (b.get("name") or "?", arg)))
        elif t == "tool_result" and full:
            c2 = b.get("content")
            if isinstance(c2, list):
                c2 = " ".join(x.get("text", "") for x in c2 if isinstance(x, dict))
            if isinstance(c2, str) and c2.strip():
                out.append(("result", " ".join(c2.split())[:300]))
    return out

turn, last = 0, ""
try:
    fh = open(tp, errors="ignore")
except OSError:
    sys.exit("digest: cannot read %s" % tp)
with fh:
    for ln in fh:
        try: o = json.loads(ln)
        except Exception: continue
        m = o.get("message")
        if not isinstance(m, dict): continue
        role = m.get("role") or o.get("type")
        # TURNS ARE COUNTED THE WAY THE METER COUNTS THEM — one per distinct assistant message
        # id — so "turn 240" here and "240t" in the status line are the same turn. Two counting
        # rules would make the covered mark skip or repeat a stretch of the session.
        if role == "assistant":
            mid = m.get("id")
            if mid and mid != last: turn += 1; last = mid
        parts = text(m.get("content"))
        if not parts: continue
        # A PROMPT IS NUMBERED FOR THE TURN IT PRODUCES, not the one before it. The counter
        # advances on the assistant's reply, so a user message read literally lands one turn
        # early — and at the covered mark that is the difference between resuming at the
        # operator's instruction and resuming at the answer to it, having dropped the
        # instruction. A user row carrying only tool results belongs to the turn that made the
        # calls, which is the one already counted.
        shown = turn + 1 if (role == "user" and any(k == "text" for k, _ in parts)) else turn
        if shown < frm: continue
        print("--- turn %d [%s]" % (shown, role))
        for kind, body in parts:
            if kind == "text":     print(body.strip())
            elif kind == "tool":   print("    · %s" % body)
            else:                  print("    > %s" % body)
PY
    ;;

state)
    sid="${2:-}"
    if [ -n "$sid" ]; then cat "$ARC/$sid.state" 2>/dev/null || echo "state=none"
    else
        for f in "$ARC"/*.state; do
            [ -e "$f" ] || { echo "no session has been archived"; break; }
            s="$(basename "$f")"; s="${s%.state}"
            [ "$s" = sweep ] && continue     # the sweep-level state is not a session
            printf '%s\t%s\t%s turn\t%s filed\n' "$s" \
                "$(state_key "$s" state)" "$(state_key "$s" at_turn)" \
                "$(state_key "$s" items_filed)"
        done
    fi
    ;;

*) sed -n '3,10p' "$0" >&2; exit 2 ;;
esac
