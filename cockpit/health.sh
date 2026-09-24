#!/usr/bin/env bash
#
# health.sh — the cockpit's right-hand column: the Spira ops dashboard.
#
#   health.sh             repaint every 2s forever (pane mode)
#   health.sh once        paint one full frame and exit
#   health.sh once <rows> [cols]
#                         paint one frame sized for a pane <rows> tall and <cols> wide, and
#                         exit — the seam the test suite drives the sizing through
#
# Reads the snapshots written by `spira/cockpit.sh` under `spira-cockpit.service`, all of them
# under $SPIRA_RUN. NEVER calls `bd` or `git` itself: the expensive sources cost seconds a pass
# and a pane that shelled out would freeze on every repaint.
#
# WHY ONLY SPIRA
# --------------
# This pane used to instrument only the predecessor harness, while the harness actually running
# the operator's work — sentinel, aeons, the landing gate — had no display at all. A dashboard
# whose subject is a decommissioned system reports on the past, and worse, trains the eye to
# skip the pane.
#
# WHAT IS ON IT, AND WHY THOSE
# ----------------------------
# The selection rule is the operator's: **instrument the numbers that surprised us.** Every line is
# something whose real value differed from what a reasonable person would assume, in a way
# that cost time. For Spira they are the ones this build itself got wrong:
#
#   born → lived     the first aeon ever summoned was killed inside the same second by the
#                    oneshot's cgroup teardown, while the sentinel reported "summoned" every
#                    two minutes into an empty log. A summon is not a worker.
#   closed → landed  CLOSED IS NOT LANDED. A bead closed with no commit naming it unblocks
#                    its dependents on a lie.
#   falseACT         CHECK 6 re-landed one branch forever because `git branch -D` refuses a
#                    branch a worktree holds and the error went to /dev/null. `acted` was
#                    therefore never 0, and CHECK 8 fires only when `acted` is 0 — so the
#                    harness could not notice it was starved. The reclaim probe had already
#                    done the same by matching its own idle message.
#   judgement        both of those bugs worked by making the judgement tier unreachable.
#                    "never fired" and "fired 40 passes ago" look identical from outside.
#   sentinel age     strand.sh cannot detect that the sentinel is dead, because a check
#                    cannot observe the failure of the thing running it. This pane can.
#
# A `?` means the probe FAILED. It never renders as 0 — a panel that reports a broken check
# as "all clear" displaces the suspicion that would have prompted a look.
#
# THE FRAME IS SIZED TO THE PANE, NOT THE OTHER WAY AROUND
# --------------------------------------------------------
# The pane is a full-height column (layout.sh splits it off the whole window), and how many
# rows that is depends on the operator's terminal. So the four sections that have more to
# say than fits — NOW, NEXT, RECENT, CI — each report every row they COULD show, and the
# renderer hands out the rows that are actually there.
#
# THE SHARE IS ROUND-ROBIN, ONE ROW AT A TIME, and that is the point. Filling each section
# in turn until the space runs out means a busy NOW pushes CI off the bottom, and CI is
# where a run that never came back is found — the section most likely to be starved is the
# one nothing else reports. Every section gets its first row before any gets its second.
#
# Sizing reads the pane's height on every repaint rather than trapping SIGWINCH, so a
# resize reflows on the next tick instead of waiting for fresh data.
#
# A SECTION WITH NOTHING TO SHOW STILL SAYS SO, and says which nothing it means: "no aeon
# working" is the harness idle, "?" is the snapshot unread. Rendering the second as the
# first is the all-clear a broken check must never be able to produce
# (law-absence-needs-a-positive-control).
#
# Lines are still emitted in priority order and the frame is still cut to the terminal's
# real height, with the count of dropped lines marked on the header: seven lines were once
# written into five and the top two scrolled away unannounced, and a dashboard that hides
# its own content is the failure mode it exists to prevent.
set -uo pipefail

# Every path comes from the harness's one configuration surface. It is two directories
# away because the cockpit ships beside the harness, not inside it.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../spira" && pwd -P)/conf.sh"

# THE FRAME IS MEASURED IN CHARACTERS, and a good half of them are multibyte — `·`, `—`,
# `↳`, `▾`, `●`. Under systemd, under a gate's `env -i`, and in any detached pane, LANG is
# unset and bash falls back to the C locale, where `${t:0:n}` counts BYTES: a title cut at a
# byte boundary ends in a broken character, and every width calculation is wrong by however
# many multibyte characters the line held. Ask for UTF-8 rather than inherit whatever
# launched the pane.
export LC_ALL="${LC_ALL:-C.UTF-8}"

# EVERY PATH HERE COMES FROM $SPIRA_RUN, NONE OF THEM DERIVED. cockpit.sh writes under
# $SPIRA_RUN, and SPIRA_RUN is configurable — a reader that recomputes the path from
# $SPIRA_REPO renders `?` for every row it feeds the moment an operator moves it, and a panel
# that reports a broken read as "nothing happening" displaces the suspicion that would have
# prompted a look.
SPIRA_SNAP="$SPIRA_RUN/cockpit.env"
# SPIRA'S OWN SERIES, beside its own snapshot: a pane drawing trends from a file nothing
# appends to draws a flat line, and a flat line reads as calm rather than absent.
HIST="$SPIRA_RUN/cockpit-history.csv"
_RENDERER_REV="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --short HEAD 2>/dev/null || true)"

C_RST=$'\e[0m'; C_DIM=$'\e[2m'; C_B=$'\e[1m'
C_OK=$'\e[32m'; C_WARN=$'\e[33m'; C_BAD=$'\e[31m'; C_ACC=$'\e[36m'

# Sparkline over the last N values of a named column of the time series.
#
# A NON-NUMERIC POINT IS DROPPED, NEVER PLOTTED AS ZERO. The series records `?` for a probe
# that failed, and drawing that as a trough turns a broken instrument into the picture of a
# quiet hour — which is the one thing a dashboard must never invent. With nothing numeric
# left it draws nothing at all, so the absence is visible instead of imagined.
spark() {
    local col="$1" n="${2:-24}"
    [ -f "$HIST" ] || { printf '%s' ""; return; }
    python3 - "$HIST" "$col" "$n" <<'PY' 2>/dev/null || printf '%s' ""
import sys, csv
path, col, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
blocks = "▁▂▃▄▅▆▇█"
try:
    with open(path) as f:
        rows = list(csv.DictReader(f))
except Exception:
    sys.exit(0)
vals = []
for r in rows[-n:]:
    try: vals.append(float(r[col]))
    except Exception: pass
if not vals: sys.exit(0)
lo, hi = min(vals), max(vals)
if hi - lo < 1e-9:
    print(blocks[0] * len(vals) if hi == 0 else blocks[3] * len(vals), end="")
else:
    print("".join(blocks[min(7, int((v - lo) / (hi - lo) * 7.999))] for v in vals), end="")
PY
}

dot() { [ "${1:-0}" = "1" ] && printf '%s●%s' "$C_OK" "$C_RST" || printf '%s○%s' "$C_BAD" "$C_RST"; }

# Render a count: green at zero, coloured above a threshold, and `?` always loud.
num() { # value warn_at label
    local v="$1" w="$2" l="${3:-}"
    if [ "$v" = "?" ]; then printf '%s%s?%s%s' "$C_BAD" "$C_B" "$C_RST" "$l"
    elif [ "$v" -ge "$w" ] 2>/dev/null; then printf '%s%s%s%s' "$C_WARN" "$v" "$C_RST" "$l"
    else printf '%s%s%s%s' "$C_OK" "$v" "$C_RST" "$l"; fi
}

# Like num, but a non-zero value is an outright fault rather than a threshold to watch:
# a stillborn aeon, an unlanded closed bead. Zero is the only healthy reading.
bad_unless_zero() {
    local v="$1" l="${2:-}"
    if [ "$v" = "?" ]; then printf '%s%s?%s%s' "$C_BAD" "$C_B" "$C_RST" "$l"
    elif [ "$v" = "0" ]; then printf '%s0%s%s' "$C_OK" "$C_RST" "$l"
    else printf '%s%s%s%s%s' "$C_BAD" "$C_B" "$v" "$C_RST" "$l"; fi
}

# fit <text> <cols> -> the text cut to <cols> characters, and MARKED when it was cut.
#
# THE PANE IS A THIRD OF THE WINDOW WIDE NOW, not half, so a line that fitted comfortably in
# the old quadrant runs off this column. Autowrap is off — a long line has to be cut rather
# than folded, or the height arithmetic is wrong by however many lines are long — and the
# terminal's own cut is silent, which is the same defect as dropping rows without saying so.
# Cutting here instead, on the plain text BEFORE any colour is added, keeps the escape
# sequences out of the arithmetic and leaves an ellipsis where the eye can see it.
#
# IT ASSIGNS TO $FIT RATHER THAN PRINTING, because `$(fit ...)` is a fork and this is called
# once per variable-length field — about fifty times a frame, on a loop that re-renders every
# two seconds to decide whether anything changed. A convenience pane must never be able to
# cost the box real work (law-fence-loops-on-shared-hardware).
FIT=""
fit() {                  # fit <text> <cols> -> sets $FIT
    local t="$1" n="$2"
    if [ "$n" -ge 2 ] 2>/dev/null && [ "${#t}" -gt "$n" ]; then FIT="${t:0:$((n-1))}…"; else FIT="$t"; fi
}

# tok <n> -> "494M" / "126k". A `?` or `-` passes through untouched: a probe that failed and a
# probe that measured nothing are different facts and neither is a number.
tok() {
    local v="${1:-?}"
    case "$v" in ''|*[!0-9]*) printf '%s' "${v:-?}"; return ;; esac
    if   [ "$v" -ge 1000000 ]; then printf '%sM' $(( v / 1000000 ))
    elif [ "$v" -ge 1000 ];    then printf '%sk' $(( v / 1000 ))
    else                            printf '%s' "$v"; fi
}

# pct <part> <whole> -> "74%", or `?` when either side is not a number or the whole is zero.
pct() {
    case "${1:-}${2:-}" in *[!0-9]*) printf '?'; return ;; esac
    [ "${2:-0}" -gt 0 ] 2>/dev/null || { printf '?'; return; }
    printf '%s%%' $(( $1 * 100 / $2 ))
}

# age <seconds> <warn> -> "12s" / "4m", coloured. `?` if the file was never written.
age_str() {
    local a="$1" w="$2" s
    [ "$a" = "?" ] && { printf '%s%s?%s' "$C_BAD" "$C_B" "$C_RST"; return; }
    if [ "$a" -lt 120 ] 2>/dev/null; then s="${a}s"; else s="$(( a / 60 ))m"; fi
    if [ "$a" -ge "$w" ] 2>/dev/null; then printf '%s%s%s' "$C_BAD" "$s" "$C_RST"
    else printf '%s%s%s' "$C_DIM" "$s" "$C_RST"; fi
}

# ======================================================================================
# SPIRA — the sections, each answering one question, each sized by the caller.
#
# Every section function emits EVERY row it could show, most important first. None of them
# knows how tall the pane is; `frame` decides that, so no section can eat the space of the
# one below it merely by being written first.
# ======================================================================================

# How many rows one elastic section may ever occupy. Past this a section stops being a
# glance and becomes a list, which is what `bd ready` is for. The collector caps its own
# output to match — there is no point emitting rows nothing can show.
MAX_SECTION_ROWS=20

# NEXT AND RECENT ARE FIVE ROWS, AND INFLOW IS THE ELASTIC ONE.
#
# All three numbers below exist because the column has slack to place and only one sensible
# place to put it. The slack itself was the earlier finding: held at five each, NEXT and
# RECENT spent the pane's slack rather than the pane — on the 52-row column this runs in an
# idle harness rendered about eighteen rows and left thirty blank, because once every section
# had reached its `want` the round-robin had nowhere left to put the budget. Raising both to
# forty fixed the blank rows and answered the question with the sixth, seventh and eighth
# entries of two ordered lists, which is the least any of those rows could have said: nobody
# reads the eighth-most-recent landing.
#
# So NEXT and RECENT are a HEIGHT again, and the slack goes to INFLOW — the newest beads in
# the store, which is the one section whose extra rows are worth having. A growing store reads
# as a busy harness whichever way it grew, and the two cases are opposite: beads arriving
# because a design was decomposed is progress, and beads arriving because one broken thing
# keeps reporting itself is a defect wearing throughput's clothes. The operator, reading an
# hour of it: "11 of the 12 beads right now are from the timed test run." (per Ryan,
# 2026-09-12)
#
# THEY STAY THREE KEYS RATHER THAN ONE. They answer different questions and will not stay the
# same number — a shared constant is how two sections come to be resized together by someone
# who meant to resize one.
#
# THE COLLECTOR MUST AGREE. A ceiling here that the collector does not emit rows for is a
# section that silently renders short; `spira/cockpit.sh` caps SP_NEXT, SP_EVENT and
# SP_INFLOW to these same figures, and the pairs are commented at each other on purpose.
#
# The generic cap above still governs CI, which is a list of work parked since yesterday and
# has no fixed useful length.
MAX_NEXT_ROWS=5
MAX_RECENT_ROWS=5
MAX_INFLOW_ROWS=40

# WHAT EACH SECTION IS GUARANTEED BEFORE ANY OF THEM IS TAKEN PAST IT. For NEXT and RECENT
# the base is also the ceiling, so a short or busy pane shows the same five queued beads and
# five events a tall idle one does. INFLOW's base is the five it must never drop below; the
# ceiling above is only what it may grow INTO once every other section is satisfied.
NEXT_BASE_ROWS=5
RECENT_BASE_ROWS=5
INFLOW_BASE_ROWS=5

# Read both snapshots into the shell. Called once per frame; every section reads what it
# left behind.
load_snapshot() {
    set +u
    [ -f "$SPIRA_SNAP" ] && . "$SPIRA_SNAP" 2>/dev/null
    set -u
}

# SNAP_ABSENT_BANNER — emitted by header_line when SPIRA_SNAP cannot be found at the
# computed path. Distinguishes the two causes a `?`-for-every-row frame could have:
#
#   PATH MISMATCH — cockpit.env exists at the other derivation of SPIRA_RUN. conf.sh derives
#     SPIRA_RUN from whether SPIRA_REPO is writable; a service installed from a writable repo
#     writes to $REPO/.runtime/spira while a reader invoked from a read-only release tree
#     derives $XDG_DATA_HOME/.../run instead. The snapshot is present but invisible here
#     (law-absence-needs-a-positive-control).
#
#   NO SNAPSHOT — no cockpit.env at any known derivation. cockpit.sh has not yet run, or the
#     service is not running on this instance.
snap_absent_banner() {
    [ -f "$SPIRA_SNAP" ] && return 0
    local _inst_sfx=""
    [ "${SPIRA_INSTANCE:-prod}" = "prod" ] || _inst_sfx="-${SPIRA_INSTANCE}"
    local _run_repo="${SPIRA_REPO}/.runtime/spira${_inst_sfx}"
    local _run_xdg="${XDG_DATA_HOME:-$HOME/.local/share}/spira${_inst_sfx}/run"
    local _alt
    for _alt in "$_run_repo" "$_run_xdg"; do
        [ "$_alt" = "$SPIRA_RUN" ] && continue
        if [ -f "$_alt/cockpit.env" ]; then
            printf '%s%s ■ PATH MISMATCH%s %s— SPIRA_RUN differs for writer and reader%s\n' \
                "$C_B" "$C_WARN" "$C_RST" "$C_DIM" "$C_RST"
            fit "$_alt" $(( COLS - 12 ))
            printf '  %sexists at%s %s\n' "$C_DIM" "$C_RST" "$FIT"
            fit "$SPIRA_RUN" $(( COLS - 12 ))
            printf '  %sreader at%s %s\n' "$C_DIM" "$C_RST" "$FIT"
            return 0
        fi
    done
    fit "$SPIRA_RUN" $(( COLS - 28 ))
    printf '%s  no snapshot — cockpit not yet run at %s%s\n' \
        "$C_DIM" "$FIT" "$C_RST"
}

# A LABELLED SECTION PER QUESTION. The old single line read
# "open 25 ready 17 working 1 aeons 1 poison 1 asks 0 fiends 0" — beads, sessions and
# asks in one undifferentiated row, so nothing could be read at a glance and the counts
# looked like they measured the same kind of thing (the operator, verbatim: "i can't tell
# at a glance what's actually happening"). Each line now answers one question and says
# which one.
# A STOPPED WORLD IS THE FIRST THING ON THE PANE, because every other figure below it is
# then a description of a system that is not running — zero aeons, zero landings and a still
# queue all render exactly as they do on a quiet, healthy afternoon. The operator, watching
# the pane through a halt: "if spira is stopped, i want that to be clear from the pane."
#
# READ FROM SYSTEMD AND THE STAMP DIRECTLY, NOT THROUGH THE SNAPSHOT. Every other row here
# comes from cockpit.env because its probes are too slow to run per repaint; these two are a
# file test and one `systemctl is-active`, and routing them through the collector would mean
# a DEAD collector renders a halted world as a running one — the failure this banner exists
# to prevent, arriving by its own back door.
#
# EITHER CONDITION IS ENOUGH. world.sh writes the stamp, but a timer stopped by hand leaves
# no stamp at all, and that world is just as stopped.
halt_banner() {
    local stamp="$SPIRA_RUN/world.halted" why="" since="" tstate
    # THROUGH THE SAME SEAM sentinel.sh ALREADY USES. A bare `systemctl` here reads the real
    # user session, so this function's output depends on whether the box's own Spira happens
    # to be running — and test-tokens.sh, which renders this pane, therefore failed two
    # assertions for the whole of the day Spira was deliberately halted to repair the gate.
    # The suite was testing the box (law-gates-run-in-a-clean-environment), and it did it in
    # the one situation where the gate most needed to be trustworthy: the gate could not go
    # green while the world was stopped, and the world was stopped in order to fix the gate.
    # PER-INSTANCE UNIT NAME resolved via spira_unit, which tries the instance-qualified form
    # first and falls back to the plain name if not found. After install.sh migrated the fleet
    # the timer is spira-sentinel-<instance>.timer and the plain name is disabled; querying the
    # plain name read "inactive" and caused the pane to announce SPIRA STOPPED while Spira was
    # running normally (2026-09-08). A health panel that reports a healthy system as stopped is
    # the same defect as one reporting a broken one as fine.
    local _su
    _su="$(spira_unit sentinel timer)"
    if [ "$_su" = '?' ]; then
        tstate=unknown
    else
        tstate="$("${SPIRA_SYSTEMCTL:-systemctl}" --user is-active "$_su" 2>/dev/null)"
        [ -n "$tstate" ] || tstate=unknown
    fi
    if [ -f "$stamp" ]; then
        since="$(head -1 "$stamp" 2>/dev/null)"
        why="$(sed -n '''2s/^why: //p''' "$stamp" 2>/dev/null)"
    elif [ "$tstate" = active ]; then
        return 0
    fi
    printf '%s%s ■ SPIRA STOPPED %s%s no aeons are summoned; nothing lands%s\n' \
        "$C_B" "$C_BAD" "$C_RST" "$C_DIM" "$C_RST"
    [ -n "$since" ] && printf '  %ssince%s %s\n' "$C_DIM" "$C_RST" "$since"
    [ -n "$why" ]   && printf '  %swhy%s   %s\n' "$C_DIM" "$C_RST" "${why:0:60}"
    [ "$tstate" != active ] && [ -z "$since" ] \
        && printf '  %ssentinel.timer is %s and no world.sh halt was recorded%s\n' "$C_DIM" "$tstate" "$C_RST"
    printf '  %sstart it:%s spira/world.sh start\n' "$C_DIM" "$C_RST"
}

# A DRAINING WORLD FOLLOWS THE HALT BANNER, because a pool held at zero by design and a pool
# that is empty because nothing is ready look identical from every other surface — and a
# forgotten drain has the same shape as law-arm-before-you-retire: a stopped channel and a
# quiet one are indistinguishable.
#
# READ THE STAMP ($SPIRA_RUN/world.draining), NOT world.sh STATUS. world.sh status reads
# systemd unit names, and those are themselves broken (sp-4biz, P0). drain and resume work
# correctly precisely because they gate on the stamp and never name a unit, so we do the same.
#
# MINUTES FROM THE STAMP'S MTIME. The stamp's first line is a human-readable timestamp; `stat
# -c %Y` extracts the epoch seconds the OS recorded when the file was written, which is the
# same instant, and arithmetic on epoch seconds needs no date parsing. If stat cannot read the
# file, the probe renders `?` — a broken read must never render as "not draining"
# (law-absence-needs-a-positive-control).
drain_banner() {
    local stamp="$SPIRA_RUN/world.draining" mtime mins
    [ -f "$stamp" ] || return 0
    mtime="$(stat -c %Y "$stamp" 2>/dev/null)"
    if [ -n "$mtime" ] && [ "$mtime" -gt 0 ] 2>/dev/null; then
        mins=$(( ($(date +%s) - mtime) / 60 ))
    else
        mins="?"
    fi
    printf '%s%s ⏸ DRAINING %s%s summons gated for %s%sm%s — loop and landing continue%s\n' \
        "$C_B" "$C_WARN" "$C_RST" "$C_DIM" "$C_RST" "$mins" "$C_DIM" "$C_RST"
    printf '  %slift it:%s world.sh resume\n' "$C_DIM" "$C_RST"
}


header_line() {
    local age="?" stale="" pass_dur=""
    [ -n "${SP_AT:-}" ] && age=$(( $(date +%s) - SP_AT ))
    if [ "$age" != "?" ] && [ "$age" -gt "${SPIRA_SNAP_STALE_S:-60}" ]; then
        # Probe timeouts explain the staleness — STALE names the cause; FAULT means unknown.
        if [ -f "$SPIRA_SNAP" ] && grep -qE "^_PROBE_STATUS_[^=]+='(timeout|error)'" "$SPIRA_SNAP" 2>/dev/null; then
            stale="  ${C_BAD}${C_B}STALE${C_RST}"
        else
            stale="  ${C_BAD}${C_B}FAULT (${age}s)${C_RST}"
        fi
    fi
    # SP_PASS_SECS: always shown in dim so a collector getting slower is visible before
    # it is a mystery. When the pass duration approaches or exceeds INTERVAL, the STALE
    # badge follows on the next snapshot — pass duration is the leading indicator.
    [ -n "${SP_PASS_SECS:-}" ] && pass_dur="  ${C_DIM}pass ${SP_PASS_SECS}s${C_RST}"
    halt_banner
    drain_banner
    # AURON IS SHOWN WITH ITS AGE, NEVER OMITTED WHEN IT IS SILENT. It is the watchdog over
    # the loop and its only power is speech, so a dead Auron and a healthy system produce
    # the same pane unless its own pulse is on it — and the pane would render the healthy
    # one (law-absence-needs-a-positive-control). `?` when it has never written a
    # heartbeat, red past three minutes against a two-minute timer.
    printf '%s%sSPIRA%s %s   %ssentinel%s %s %ss  %sops%s %s %ss  %sauron%s %s %s%s%s\n' \
        "$C_B" "$C_ACC" "$C_RST" "$(date +%H:%M)" \
        "$C_DIM" "$C_RST" "$(dot "${SP_SENTINEL_TIMER:-0}")" "${SP_SENTINEL_AGE:-?}" \
        "$C_DIM" "$C_RST" "$(dot "${SP_OPS_TIMER:-0}")" "${SP_OPS_AGE:-?}" \
        "$C_DIM" "$C_RST" "$(dot "${SP_AURON_TIMER:-0}")" \
        "$(age_str "${SP_AURON_AGE:-?}" 180)" "$stale" "$pass_dur"

    # WHAT AURON IS SAYING, on its own line and only when it is saying something. An alert
    # is a condition that self-clears, so an empty line here is the ordinary case and must
    # cost no space; a `?` is not empty, though — it means the heartbeat could not be read,
    # which is a different thing from "nothing is wrong" and says so.
    if [ "${SP_AURON_FIRING:-0}" = "?" ]; then
        printf ' %sALERT%s  %s%s? — Auron'"'"'s heartbeat could not be read%s\n' \
            "$C_DIM" "$C_RST" "$C_BAD" "$C_B" "$C_RST"
    elif [ "${SP_AURON_FIRING:-0}" != "0" ]; then
        printf ' %sALERT%s  %s%s%s firing%s  %s%s%s\n' \
            "$C_DIM" "$C_RST" "$C_BAD" "$C_B" "${SP_AURON_FIRING}" "$C_RST" \
            "$C_BAD" "$(printf '%s' "${SP_AURON_KEYS:-}" | tr ',' ' ')" "$C_RST"
    fi
    if [ -f "$SPIRA_SNAP" ]; then
        local _probe_info="" _pline _pname _pkilled
        while IFS= read -r _pline; do
            _pname="${_pline%%=*}"; _pname="${_pname#_PROBE_STATUS_}"
            _pkilled="$(grep "^SP_PROBE_KILLED_${_pname}=" "$SPIRA_SNAP" 2>/dev/null \
                | cut -d= -f2 | tr -d "'")"
            _probe_info="${_probe_info:+$_probe_info  }${_pname}: timeout ×${_pkilled:-?}"
        done < <(grep -E "^_PROBE_STATUS_[^=]+='(timeout|error)'" "$SPIRA_SNAP" 2>/dev/null)
        if [ -n "$_probe_info" ]; then
            printf ' %sSTALE%s  %s%s%s%s\n' \
                "$C_DIM" "$C_RST" "$C_BAD" "$C_B" "$_probe_info" "$C_RST"
        fi
    else
        snap_absent_banner
    fi
}

# TOKENS — what the account is spending, and which half of the system is spending it.
#
# DIRECTLY UNDER THE HEADER AND OUTSIDE THE SHARE, because it is the constraint on everything
# below it: when the account is out of capacity no aeon can be summoned and no bead can move,
# so every other figure on this pane freezes for a reason nothing else here reports. It is the
# leading indicator of the GOV row's "ACCOUNT OUT OF CAPACITY", which only says so once it is
# too late to act. Fixed height by the same rule as the standing figures — each row is one
# number that is always worth its row — so the elastic sections never elide it.
#
# THE SPLIT IS THE WHOLE POINT, and it gets a row each. A single total would not have answered
# the question this was built for, and the answer was not the obvious one: the interactive
# session is about three quarters of the window, so optimising the harness first would have
# been optimising the smaller half. One half per row rather than both on one, because this is
# a column about a third of the window wide and the tail of a long line is silently cut — the
# tail here being the session, which is the larger half and the finding.
#
# CONTEXT PER TURN, NOT OUTPUT. What a rate limit meters is everything a request carried, and
# the whole context is re-read on EVERY turn: cache reads outweigh output here by roughly 175
# to 1. The lever is context size multiplied by turns, so both are on the row.
tokens_section() {
    printf ' %sTOKENS/%sh%s  %s%s%s billed  %s%s%s\n' \
        "$C_DIM" "${SP_TOK_WINDOW_H:-?}" "$C_RST" \
        "$C_B" "$(tok "${SP_TOK_WIN:-?}")" "$C_RST" \
        "$C_ACC" "$(spark tok_win $(( COLS > 46 ? (COLS - 34 > 24 ? 24 : COLS - 34) : 0 )))" "$C_RST"
    local _coll_rev="${SP_COLLECTOR_REV:-}"
    local half
    for half in AEON:aeons ARC:archivist SESS:session; do
        eval "local w=\${SP_TOK_${half%%:*}_WIN:-?} t=\${SP_TOK_${half%%:*}_TURNS:-?} c=\${SP_TOK_${half%%:*}_CTX:-?}"
        if [ "$w" = "?" ] && [ -n "$_coll_rev" ] && [ "$_coll_rev" != "$_RENDERER_REV" ]; then
            printf '        %s%-8s%s %scoll %s%s\n' \
                "$C_DIM" "${half#*:}" "$C_RST" "$C_DIM" "$_coll_rev" "$C_RST"
        else
            # Fitted like every other variable-length field: on a narrow column the tail is
            # what the terminal would cut, and here the tail is the context-per-turn.
            fit "${t}t · $(tok "$c") ctx/turn" $(( COLS - 22 ))
            printf '        %s%-8s%s %-4s %s%s%s\n' \
                "$C_DIM" "${half#*:}" "$C_RST" "$(pct "$w" "${SP_TOK_WIN:-?}")" \
                "$C_DIM" "$FIT" "$C_RST"
        fi
    done

    # RATE LIMIT WINDOWS. The two windows that end a working day when full, now visible before
    # that happens. SP_RATELIM_5H_PCT / _7D_PCT are 0–100 integers from the newest
    # rate_limit_event across live aeon traces. SP_RATELIM_*_MIN is minutes until reset.
    # SP_RATELIM_*_ETA is seconds to full from the last hour's slope ('-' resets first,
    # '?' no slope). SP_RATELIM_AGE is seconds since the newest trace reading.
    #
    # A MISSING READING RENDERS '?', NOT 0. A window shown at 0% is the best possible news;
    # a broken probe must never produce that reading (law-absence-needs-a-positive-control).
    #
    # COLOUR BANDS match ctx-meter.sh: green < 70%, yellow 70–90%, red ≥ 90%.
    local p5="${SP_RATELIM_5H_PCT:-?}" p7="${SP_RATELIM_7D_PCT:-?}"
    local m5="${SP_RATELIM_5H_MIN:-?}" m7="${SP_RATELIM_7D_MIN:-?}"
    local e5="${SP_RATELIM_5H_ETA:-?}" e7="${SP_RATELIM_7D_ETA:-?}"
    local cage="${SP_RATELIM_AGE:-?}"

    # Colour for each window
    local c5="$C_OK" c7="$C_OK"
    [ "${p5:-0}" -ge 90 ] 2>/dev/null && c5="${C_BAD}${C_B}" || \
    [ "${p5:-0}" -ge 70 ] 2>/dev/null && c5="$C_WARN"
    [ "${p7:-0}" -ge 90 ] 2>/dev/null && c7="${C_BAD}${C_B}" || \
    [ "${p7:-0}" -ge 70 ] 2>/dev/null && c7="$C_WARN"

    # Minutes → human-readable duration. Pure-bash, no subshell.
    local DUR5 DUR7
    _dur_m() {  # _dur_m <minutes> <varname>
        local _m="$1" _v="$2"
        case "$_m" in ''|'?'|*[!0-9]*) printf -v "$_v" '%s' "${_m:-?}"; return ;; esac
        if   [ "$_m" -eq 0 ];    then printf -v "$_v" '0m'
        elif [ "$_m" -lt 60 ];   then printf -v "$_v" '%dm' "$_m"
        elif [ "$_m" -lt 1440 ]; then printf -v "$_v" '%dh%dm' $(( _m / 60 )) $(( _m % 60 ))
        else printf -v "$_v" '%dd%dh' $(( _m / 1440 )) $(( _m % 1440 / 60 )); fi
    }
    _dur_m "$m5" DUR5; _dur_m "$m7" DUR7

    # ETA suffix for each window (empty when not projectable)
    local eta5_sfx="" eta7_sfx=""
    case "$e5" in
        '-'|'?'|'') ;;
        '0') eta5_sfx=" ${C_BAD}${C_B}FULL${C_RST}" ;;
        *[!0-9]*) ;;
        *)  local _em5=$(( e5 / 60 ))
            local _ed5
            _dur_m "$_em5" _ed5
            eta5_sfx=" ${C_DIM}→full ${_ed5}${C_RST}" ;;
    esac
    case "$e7" in
        '-'|'?'|'') ;;
        '0') eta7_sfx=" ${C_BAD}${C_B}FULL${C_RST}" ;;
        *[!0-9]*) ;;
        *)  local _em7=$(( e7 / 60 ))
            local _ed7
            _dur_m "$_em7" _ed7
            eta7_sfx=" ${C_DIM}→full ${_ed7}${C_RST}" ;;
    esac

    # Stale-reading note: if the newest trace sample is more than 15 minutes old, say so.
    local age_sfx=""
    [ "${cage:-0}" -ge 900 ] 2>/dev/null \
        && age_sfx=" ${C_DIM}($(( cage / 60 ))m ago)${C_RST}"

    # FOUR SPACES, NOT THREE. Every section on this pane starts its content at column 9 —
    # ` RECENT ` and `        ` alike — and this row started it at 8, so the 5h figure sat one
    # column left of the 7d figure directly beneath it in the one place the two are read
    # against each other. test-now.sh now asserts the column for every label.
    printf ' %sWIN%s    5h %s%s%%%s  %sreset %s%s%s%s\n' \
        "$C_DIM" "$C_RST" \
        "$c5" "${p5:-?}" "$C_RST" \
        "$C_DIM" "$DUR5" "$C_RST" \
        "$eta5_sfx" "$age_sfx"
    printf '        7d %s%s%%%s  %sreset %s%s%s\n' \
        "$c7" "${p7:-?}" "$C_RST" \
        "$C_DIM" "$DUR7" "$C_RST" \
        "$eta7_sfx"
    return 0
}

# `<label> ? <why>` — the shape every section uses when its input could not be read. It is
# deliberately not the shape of "nothing to report": a broken read that renders as an
# all-clear displaces the suspicion that would have prompted a look, and these two states
# are one unset variable apart (law-absence-needs-a-positive-control).
unread_row() {   # unread_row <label> <what could not be read>
    printf ' %s%-6s%s %s%s?%s %s%s%s\n' \
        "$C_DIM" "$1" "$C_RST" "$C_BAD" "$C_B" "$C_RST" "$C_DIM" "$2" "$C_RST"
}

# model_short <id> -> a name that fits a third-width pane: claude-opus-4-6 -> opus 4.6.
#
# THE UNKNOWNS PASS STRAIGHT THROUGH. `?` means the collector could not read the trace and `-`
# means the trace held no init event; neither is turned into a plausible model name, because a
# pane that invents one is worse than a pane that admits it does not know
# (law-absence-needs-a-positive-control). Anything this does not recognise is printed as it
# came, so a new model id shows up ugly rather than silently as something else.
model_short() {
    case "${1:-?}" in
        ""|"?"|"-") printf '%s' "${1:-?}" ;;
        # THE DATE STAMP COMES OFF FIRST. `claude-haiku-4-5-20251001` otherwise matches the
        # version rule on its last two groups and renders "haiku-4 5.20251001", which reads as
        # a version number nobody has. A dated id is the normal shape for a pinned model, so
        # this is the common case rather than a curiosity.
        claude-*)   printf '%s' "${1#claude-}" \
                      | sed -E 's/-[0-9]{8}$//; s/-([0-9]+)-([0-9]+)$/ \1.\2/; s/-([0-9]+)$/ \1/' ;;
        *)          printf '%s' "$1" ;;
    esac
}

# Count aeons that are genuinely live right now, from /proc. This is cheap (file reads only)
# and exact: unlike the snapshot count, it sees aeons that started or stopped during the
# collector's 120s pass. The same /proc check lib.sh's aeon_alive runs; duplicated here
# because health.sh does not source lib.sh (law-absence-needs-a-positive-control).
#
# Capture cmdline THEN match: `tr | grep -q` under pipefail returns 141 on the first
# match (grep closes the read end of the pipe, the writer dies of SIGPIPE), so the live
# case is exactly the one that could read as dead (law-no-grep-q-under-pipefail).
_live_aeon_n() {
    local _n=0 _pf _pid _cmd
    for _pf in "$SPIRA_RUN"/aeon-*.pid; do
        [ -f "$_pf" ] || continue
        _pid="$(cat "$_pf" 2>/dev/null)"; [ -n "${_pid:-}" ] || continue
        [ -d "/proc/$_pid" ] || continue
        { _cmd="$(tr '\0' ' ' < "/proc/$_pid/cmdline")"; } 2>/dev/null
        grep -qF 'aeon.sh' <<< "${_cmd:-}" && _n=$((_n+1))
    done
    printf '%d' "$_n"
}

# NOW — who is working, on what, HOW THE SESSION IS DOING, and the last thing it said.
#
# FOUR ROWS PER AEON, because three of them answered "is it alive" and none answered "is it
# well". A name and a last command cannot distinguish a session forty turns in and near its
# context ceiling from one that started a minute ago, nor either from one that stopped
# writing to its trace twenty minutes back — and that last case is the one worth catching,
# since it is the state the stall detector is about to act on. The figures come from the
# collector, which reads each trace once a pass; this section only formats them.
#
# It is therefore the section that grows fastest, and the one the round-robin share exists to
# keep in its lane. It is NOT the section the generic cap trims — see `frame`.
#
# LIVE COUNT FIRST: a live /proc read for the aeon count, because the snapshot is up to 120s
# stale when it lands. A 182s aeon can start and finish inside one pass — it reads as absent
# for its whole life. An aeon that started just after the collector's NOW loop also reads as
# absent. The snapshot data (per-aeon rows) still comes from the snapshot because trace stats
# require a file read the pane cannot afford on every repaint; the COUNT is cheap and is
# always current (law-absence-needs-a-positive-control, sp-e9sbi).
now_section() {
    local _live_n
    _live_n="$(_live_aeon_n)"

    if [ -z "${SP_AEON_N:-}" ] || [ "${SP_AEON_N:-}" = "?" ]; then
        # Snapshot unreadable — fall back to live count if any are visible.
        if [ "${_live_n:-0}" -gt 0 ] 2>/dev/null; then
            printf ' %sNOW%s    %s%s aeon(s) live%s %s— details pending snapshot%s\n' \
                "$C_DIM" "$C_RST" "$C_OK$C_B" "$_live_n" "$C_RST" "$C_DIM" "$C_RST"
        else
            unread_row NOW "cannot read the aeon roster"
        fi
        return
    fi

    local _snap_n="${SP_AEON_N}"

    # Neither the snapshot nor /proc show any aeons.
    if [ "$_snap_n" -eq 0 ] 2>/dev/null && [ "$_live_n" -eq 0 ] 2>/dev/null; then
        printf ' %sNOW%s    %sno aeon working%s\n' "$C_DIM" "$C_RST" "$C_DIM" "$C_RST"
        return
    fi

    # INVERSION CASE: /proc has aeons the snapshot does not know about. They started during
    # the collector's pass, after it stamped SP_AEON_N=0. Show the live count; per-aeon
    # detail rows arrive on the next pass when the snapshot catches up.
    if [ "$_snap_n" -eq 0 ] 2>/dev/null && [ "$_live_n" -gt 0 ] 2>/dev/null; then
        printf ' %sNOW%s    %s%s aeon(s) live%s %s— not yet in snapshot%s\n' \
            "$C_DIM" "$C_RST" "$C_OK$C_B" "$_live_n" "$C_RST" "$C_DIM" "$C_RST"
        return
    fi

    local i=0
    while [ "$i" -lt "${SP_AEON_N}" ]; do
        eval "local nm=\${SP_AEON${i}_NAME:-?} fy=\${SP_AEON${i}_FAYTH:-?}"
        eval "local bd=\${SP_AEON${i}_BEAD:-?} mn=\${SP_AEON${i}_MIN:-?} ac=\${SP_AEON${i}_ACT:-}"
        eval "local ti=\${SP_AEON${i}_TITLE:-} pr=\${SP_AEON${i}_PRI:-?} pa=\${SP_AEON${i}_PARTITION:-?}"
        # EVERY ONE OF THESE DEFAULTS TO `?`, NEVER TO 0 OR TO EMPTY. An unset key here means
        # the collector could not read the trace, and a session whose trace cannot be read
        # rendering as "0 turns, 0 files" is an all-clear that displaces the suspicion which
        # would have prompted a look (law-absence-needs-a-positive-control).
        eval "local tn=\${SP_AEON${i}_TURNS:-?} cx=\${SP_AEON${i}_CTX:-?}"
        eval "local md=\${SP_AEON${i}_MODEL:-?}"
        eval "local fm=\${SP_AEON${i}_FAYTH_MODEL:-?}"
        eval "local fl=\${SP_AEON${i}_FILES:-?} qt=\${SP_AEON${i}_QUIET:-?}"
        eval "local sd=\${SP_AEON${i}_SAID:-}"
        # WHO, then WHAT, then the live action — one question per line. Crammed onto
        # one row the name, the bead, the elapsed time and a shell command ran past the
        # pane width and truncated mid-word.
        #
        # THE STATS SHARE THE NAME'S ROW because they describe the same thing the name does:
        # the session, not the bead. Only the tail is fitted — the name is the one field on
        # this row that must never be cut, since it is how two aeons are told apart.
        # THE MODEL, BECAUSE THE PERSONAE NO LONGER SHARE ONE. Ops runs Haiku 4.5, the
        # builders Sonnet 4.6, spike Opus 5, and a bead that went slowly or answered oddly is a
        # different fact depending on which was behind it. It sits on the
        # NAME's row rather than the bead's because a model is a property of the SESSION, like
        # the turns and the context beside it, and not of the work.
        #
        # IT IS READ FROM THE AEON'S OWN TRACE, never from the fayth file. The two disagree
        # exactly when it matters — a fayth edited mid-flight leaves the running session on
        # the model it was summoned with — and a pane sourcing the file would relabel live
        # work at the moment somebody is looking (law-long-lived-processes-pin-their-config).
        #
        # FAYTH_MODEL IS THE DECLARED MODEL — what the persona file says to request. When the
        # trace MODEL and FAYTH_MODEL both resolve to real values but differ, a provider
        # substitution occurred silently: the summon ran a different model than was asked for.
        # The marker ⚠ followed by the declared model makes the mismatch visible so the
        # operator can see which model actually ran. An unreadable value renders `?` — it is
        # never folded into a match (law-absence-needs-a-positive-control).
        local model_disp
        model_disp="$(model_short "$md")"
        if [ "$md" != "?" ] && [ "$md" != "-" ] && [ "$fm" != "?" ] && [ "$fm" != "-" ] && [ "$md" != "$fm" ]; then
            model_disp="$model_disp ⚠$(model_short "$fm")"
        fi
        fit "the $fy on $model_disp · ${mn}m · $tn turns · ctx $(tok "$cx") · $fl files" \
            $(( COLS - 9 - ${#nm} ))
        printf ' %s%s%s    %s%s%s %s%s%s\n' \
            "$C_DIM" "$([ "$i" = 0 ] && printf 'NOW' || printf '   ')" "$C_RST" \
            "$C_OK$C_B" "$nm" "$C_RST" "$C_DIM" "$FIT" "$C_RST"
        # Same shape as a NEXT row, deliberately: id in the accent colour, title dim,
        # so a bead being worked and a bead about to be worked read as the same kind of
        # thing in the same place on the line.
        # FOURTEEN COLUMNS OF ID, not twenty-two. Every row under a label starts at the
        # label column, which is eight; a wider id field on top of that was spending more
        # of a third-width pane on alignment than on the title the alignment exists to line
        # up. The budget is computed rather than constant, as in `next_row`.
        # P<n> FIRST, exactly as next_row and the RECENT rows lead — the three sections
        # describe the same beads at three stages of one lifecycle, and until this was here
        # they could not be compared down the column.
        # LEASE: time left in the liveness lease, counting down as the trace stops growing.
        # Renewed by any trace write (tool_progress, model output). "?" when the deadline
        # file cannot be read — never 0, which would look like "about to expire" rather
        # than "probe failed" (law-absence-needs-a-positive-control).
        eval "local _lease=\${SP_AEON${i}_LEASE:-?}"
        local lease_disp lease_col
        if [ "$_lease" = "?" ]; then
            lease_disp="?"; lease_col="$C_DIM"
        elif [ "$_lease" -lt 3 ] 2>/dev/null; then
            lease_disp="${_lease}m"; lease_col="$C_BAD$C_B"
        elif [ "$_lease" -lt 5 ] 2>/dev/null; then
            lease_disp="${_lease}m"; lease_col="$C_WARN"
        else
            lease_disp="${_lease}m"; lease_col="$C_DIM"
        fi
        local pw=8; [ "${#pa}" -gt "$pw" ] && pw=${#pa}
        local lease_w="${#lease_disp}"
        fit "${ti:-?}" $(( COLS - 13 - pw - (${#bd} > 14 ? ${#bd} : 14) - 1 - lease_w ))
        printf '        %sP%s%s %s%-*s%s %s%-14s%s %s%s%s %s%s%s\n' \
            "$(pri_colour "P$pr")" "$pr" "$C_RST" \
            "$C_DIM" "$pw" "$pa" "$C_RST" \
            "$C_ACC" "$bd" "$C_RST" "$C_DIM" "$FIT" "$C_RST" \
            "$lease_col" "$lease_disp" "$C_RST"
        # THE ACTION AND THE SILENCE ON ONE ROW, at opposite ends of it. They are one fact
        # read together and useless read apart: `Bash gh run watch` quiet for eleven minutes
        # is a session waiting correctly, and the same command quiet for ten minutes is the
        # one whose lease is about to lapse. 300s is where a silence stops being ordinary
        # thinking; 600s is the lease duration, after which the aeon is killed.
        local qs qcol
        if [ "$qt" = "?" ] || [ "$qt" = "-" ]; then qs="quiet $qt"; qcol="$C_BAD$C_B"
        else
            if [ "$qt" -lt 120 ] 2>/dev/null; then qs="quiet ${qt}s"; else qs="quiet $(( qt / 60 ))m"; fi
            if   [ "$qt" -ge 1200 ] 2>/dev/null; then qcol="$C_BAD$C_B"
            elif [ "$qt" -ge 300 ]  2>/dev/null; then qcol="$C_WARN"
            else                                      qcol="$C_DIM"; fi
        fi
        # TRAILING MOMENTS — the N most recent things the session did, oldest first, from
        # the collector's trace_tail read. The newest carries the quiet indicator; the older
        # ones are dim context answering "what led here". When the snapshot holds no ACT{j}
        # keys — N=0, or an older collector — the single ACT line and the SAID line render
        # as before.
        # TWO, AND conf.sh AGREES. The fallback is here for a pane running without a
        # sourced conf, so a disagreement between the two would show up as a section that
        # is a row taller on one box than on another.
        local tl="${SPIRA_COCKPIT_TRACE_LINES:-2}" trail_n=0 trail_have=0
        if [ "$tl" -gt 0 ] 2>/dev/null; then
            # COUNT WHAT THE SNAPSHOT HOLDS FIRST, THEN KEEP THE NEWEST OF IT. Reading the
            # first `tl` keys is the obvious loop and it drops the wrong end: the collector
            # writes ACT0..ACTn-1 oldest-first, so a snapshot holding three moments and a
            # pane rendering two showed the FIRST two — the last thing the session did was
            # the row thrown away, and the `quiet` timer, which belongs to that moment, was
            # printed against the one before it. The two numbers agree in steady state and
            # disagree for exactly as long as a stale snapshot outlives a changed setting,
            # which is when a reader is most likely to be looking.
            while :; do
                eval "[ -n \"\${SP_AEON${i}_ACT${trail_have}:-}\" ]" || break
                trail_have=$((trail_have+1))
            done
            trail_n="$trail_have"
            [ "$trail_n" -gt "$tl" ] && trail_n="$tl"
        fi
        if [ "$trail_n" -gt 0 ]; then
            local j=$(( trail_have - trail_n ))
            while [ "$j" -lt "$trail_have" ]; do
                eval "local trail_line=\${SP_AEON${i}_ACT${j}:-}"
                if [ "$j" -eq $((trail_have - 1)) ]; then
                    local aw=$(( COLS - 12 - ${#qs} )); [ "$aw" -lt 2 ] && aw=2
                    fit "${trail_line:--}" "$aw"
                    local pad=$(( COLS - 10 - ${#FIT} - ${#qs} )); [ "$pad" -lt 1 ] && pad=1
                    printf '        %s↳ %s%s%*s%s%s%s\n' \
                        "$C_RST" "$FIT" "$C_RST" "$pad" "" "$qcol" "$qs" "$C_RST"
                else
                    fit "$trail_line" $(( COLS - 12 ))
                    printf '        %s↳ %s%s\n' "$C_DIM" "$FIT" "$C_RST"
                fi
                j=$((j+1))
            done
        else
            # No trailing moments: original single-line behaviour.
            local aw=$(( COLS - 12 - ${#qs} )); [ "$aw" -lt 2 ] && aw=2
            fit "${ac:--}" "$aw"
            local pad=$(( COLS - 10 - ${#FIT} - ${#qs} )); [ "$pad" -lt 1 ] && pad=1
            printf '        %s↳ %s%s%*s%s%s%s\n' \
                "$C_DIM" "$FIT" "$C_RST" "$pad" "" "$qcol" "$qs" "$C_RST"
            # WHAT IT LAST SAID, IN ITS OWN WORDS — kept only in single-line mode.
            # When trailing moments are shown the said text appears naturally among them.
            case "$sd" in ''|'-'|'?') ;; *)
                fit "$sd" $(( COLS - 12 ))
                printf '        %s“ %s ”%s\n' "$C_DIM" "$FIT" "$C_RST" ;;
            esac
        fi
        i=$((i+1))
    done

    # If more aeons are live in /proc than the snapshot captured, the extras started during
    # the collector pass. Show a one-line notice so the operator sees them immediately
    # rather than on the next snapshot (which may be 60–120s away).
    local _extra=$(( _live_n - _snap_n ))
    if [ "$_extra" -gt 0 ] 2>/dev/null; then
        printf '        %s+%d more aeon(s) live — not yet in snapshot%s\n' \
            "$C_DIM" "$_extra" "$C_RST"
    fi
}

# "P0 sp-id Title..." -> aligned id + dim title, matching NOW.
#
# THE TITLE BUDGET IS COMPUTED, NOT CONSTANT. A `%-14s` field occupies fourteen columns, or
# the id's own length when it is longer — printf pads a short field and never truncates a
# long one, and truncating an id is not an option because half an id is not an id. So the
# prefix width varies per row and the space left for the title varies with it. The ternary
# is written inline rather than as a helper for the same no-forks reason as `fit`.
# ONE VOCABULARY OF COLOUR FOR THE WHOLE COLUMN, because NOW, NEXT and RECENT show the same
# beads at three stages and a reader scans down, not across. P0 and a reopen must look alike
# wherever they appear, or the colour is decoration rather than information.
pri_colour() {          # pri_colour P0 -> the escape for that priority
    case "$1" in
        P0) printf '%s' "$C_BAD$C_B" ;;
        P1) printf '%s' "$C_WARN" ;;
        *)  printf '%s' "$C_DIM" ;;
    esac
}

# A LIFECYCLE VERB IS GREEN WHEN WORK ADVANCED, RED WHEN IT WENT BACKWARDS, AMBER WHEN IT
# ENDED WITHOUT EITHER. The middle case is the one worth seeing: an aeon whose turn ended with
# its bead still in progress is neither a success nor a failure, and it is what repeats.
verb_colour() {
    case "$1" in
        landed|finished|announced)      printf '%s' "$C_OK" ;;
        reopened|poisoned|slain|reaped) printf '%s' "$C_BAD" ;;
        in_progress|ended|reclaimed)    printf '%s' "$C_WARN" ;;
        claimed)                        printf '%s' "$C_ACC" ;;
        *)                              printf '%s' "$C_DIM" ;;
    esac
}

part_colour() {
    case "$1" in
        ops) printf '%s' "$C_WARN" ;;
        *)   printf '%s' "$C_DIM"  ;;
    esac
}

# A KIND IS COLOURED BY WHAT ITS ARRIVAL MEANS, which is the only reason INFLOW shows the
# kind at all. A bug or an incident flowing in is the system reporting on itself, and a run
# of them is the shape a defect makes when it reproduces once per suite file. A decision is a
# question that will sit in the store until Ryan answers it. A task or an epic is ordinary
# planned work and wears the same dim as everything else on the column.
kind_colour() {
    case "$1" in
        bug|incident) printf '%s' "$C_BAD" ;;
        decision)     printf '%s' "$C_WARN" ;;
        *)            printf '%s' "$C_DIM" ;;
    esac
}

next_row() {
    local raw="$1" pri part id rest pw
    pri="${raw%% *}"; rest="${raw#* }"; part="${rest%% *}"; rest="${rest#* }"; id="${rest%% *}"; rest="${rest#* }"
    pw=8; [ "${#part}" -gt "$pw" ] && pw=${#part}
    fit "$rest" $(( COLS - 11 - ${#pri} - pw - (${#id} > 14 ? ${#id} : 14) ))
    printf '        %s%s%s %s%-*s%s %s%-14s%s %s%s%s\n' \
        "$(pri_colour "$pri")" "$pri" "$C_RST" \
        "$C_DIM" "$pw" "$part" "$C_RST" \
        "$C_ACC" "$id" "$C_RST" "$C_DIM" "$FIT" "$C_RST"
}

# "P1 sp-id 12m Title..." -> next_row's format plus an age since close, coloured by threshold.
# Age past hours is the actionable signal; the count alone is the normal healthy state of a
# working system, and colouring it would make this section wallpaper within a day.
queue_row() {           # queue_row "P<pri> <id> <title>"
    local raw="$1" pri id rest
    pri="${raw%% *}"; rest="${raw#* }"; id="${rest%% *}"; rest="${rest#* }"
    fit "$rest" $(( COLS - 10 - ${#pri} - (${#id} > 14 ? ${#id} : 14) ))
    printf '        %s%s%s %s%-14s%s %s%s%s\n' \
        "$(pri_colour "$pri")" "$pri" "$C_RST" "$C_ACC" "$id" "$C_RST" \
        "$C_DIM" "$FIT" "$C_RST"
}

# RECENT rows arrive as `<age> <actor> <verb> <bead> <title...>` and are printed in the
# operator order — TIME, STATE, PERSONA, BEAD, DESCRIPTION. The verb moves ahead of the
# persona because the state is what a reader scans this section for; the persona answers
# "whose" once the eye has stopped. Split rather than print flat. A row that cannot be
# parsed is printed as-is.
#
# THE COLLECTOR NO LONGER EMITS A PARTITION and no longer suffixes the actor with the aeon
# assignee: both said the same word the fayth name already said, and the truncation kept the
# repetition while dropping the aeon name that made it worth having.
recent_row() {          # recent_row "<age> <actor> <verb> <bead> <title>" <indent-cols>
    local raw="$1" pad="$2" age actor verb id rest aw vw pw
    # TWO FIXED FIELDS then body. The age is one word — "52s", not "52s ago" — so the leading
    # group ends at the unit letter. Body is verb + bead (or count) + optional title.
    if [[ "$raw" =~ ^([0-9]+[smhd])[[:space:]]+([^[:space:]]+)[[:space:]]+([^[:space:]]+)[[:space:]]+([^[:space:]]+)[[:space:]]*(.*)$ ]]; then
        age="${BASH_REMATCH[1]}"; actor="${BASH_REMATCH[2]}"
        verb="${BASH_REMATCH[3]}"; id="${BASH_REMATCH[4]}"; rest="${BASH_REMATCH[5]}"
    else
        # UNPARSED IS PRINTED AS IT CAME. A formatter must never drop content it could not
        # split — the event is the load-bearing half and the colour is the ornament.
        # IT IS STILL CUT TO THE PANE. Autowrap is off, so a row wider than the pane is cut by
        # the terminal instead, and the terminal's cut is silent — the one thing this pane must
        # never do. Failing to parse a row is not a licence to overflow the column.
        fit "$raw" $(( COLS - pad ))
        printf '%s%s%s\n' "$C_DIM" "$FIT" "$C_RST"; return
    fi
    # THE FIFTH FIELD IS NOT ALWAYS A BEAD. Some sentinel ACTs are AGGREGATES over a pass —
    # "reaped 1 landed branch(es)", "escalated 3 stranded item(s)", "announced and ..." —
    # where the word after the verb is a COUNT or a preposition. Dropped into the id column
    # it wore the same accent colour every real id wears, so "reaped 1" read as a bead named
    # `1` and the operator went looking for it. A bead reference is an `sp-` id, optionally
    # carrying its branch prefix (`spira/sp-d0c2`); anything else is prose, and prose is
    # printed as prose across the id and title columns rather than being cut in half by them.
    aw=${#age};   if [ "$aw" -lt 4 ]; then aw=4; fi
    # THE VERB IS PADDED, like the age and the persona either side of it. Unpadded, the
    # columns after it began wherever the verb happened to end — `ended` to `reclaimed` is
    # five columns of drift in the one section whose purpose is to be scanned straight down.
    # Nine covers every verb the sentinel logs and the common aeon outcomes. It is NOT the
    # longest string that can land here — an aeon that dies on `unmapped-repo` writes its
    # thirteen-character status as the verb — and such a row widens itself rather than being
    # cut, so the arithmetic below takes whichever is greater. Padding to thirteen would
    # spend four columns on every row for a state that is a defect when it appears, and a
    # row that breaks the column is a fair way for a defect to announce itself. Same for the
    # persona: eight covers sentinel, overseer and every fayth name in the chamber.
    vw=${#verb};  if [ "$vw" -lt 9 ]; then vw=9; fi
    pw=${#actor}; if [ "$pw" -lt 8 ]; then pw=8; fi
    if [[ ! "$id" =~ (^|/)sp-[A-Za-z0-9._-]+$ ]]; then
        rest="$id${rest:+ $rest}"
        # fit width: aw+1+vw+1+pw+1+fit = COLS-pad  →  fit = COLS - pad - aw - vw - pw - 3
        fit "$rest" $(( COLS - pad - aw - vw - pw - 3 ))
        printf '%s%-*s%s %s%-*s%s %s%-*s%s %s%s%s\n' \
            "$C_DIM" "$aw" "$age" "$C_RST" \
            "$(verb_colour "$verb")" "$vw" "$verb" "$C_RST" \
            "$(part_colour "$actor")" "$pw" "$actor" "$C_RST" \
            "$C_DIM" "$FIT" "$C_RST"
        return
    fi
    # fit width: aw+1+vw+1+pw+1+max(14,id)+1+fit = COLS-pad
    # → fit = COLS - pad - aw - vw - pw - max(14, id_len) - 4
    fit "$rest" $(( COLS - pad - aw - vw - pw - (${#id} > 14 ? ${#id} : 14) - 4 ))
    printf '%s%-*s%s %s%-*s%s %s%-*s%s %s%-14s%s %s%s%s\n' \
        "$C_DIM" "$aw" "$age" "$C_RST" \
        "$(verb_colour "$verb")" "$vw" "$verb" "$C_RST" \
        "$(part_colour "$actor")" "$pw" "$actor" "$C_RST" \
        "$C_ACC" "$id" "$C_RST" \
        "$C_DIM" "$FIT" "$C_RST"
}

# NEXT — the order the graph will actually be claimed in.
# CLAIM ORDER, not "the top of each priority". These are literally the next beads
# `bd ready --claim` would take, in that order. The count on the header is the WHOLE
# queue; the rows beneath it are as much of the head of that queue as the pane can hold.
next_section() {
    if [ -z "${SP_NEXT_N:-}" ] || [ "${SP_NEXT_N:-}" = "?" ]; then
        unread_row NEXT "cannot read the ready queue"
        return
    fi
    # N ready · M reachable [· K stranded]: the three numbers that say whether the loop
    # will drain everything on its own, or stall on something that needs a human.
    # SP_REACHABLE=? means the probe failed — show ? rather than omitting the column.
    # SP_STRANDED=0 or ? suppresses the stranded clause (0 is normal, ? is unread).
    local _reach="${SP_REACHABLE:-?}" _strand="${SP_STRANDED:-?}"
    local _count_line="${SP_NEXT_N} ready · ${_reach} reachable"
    if [ "$_strand" != "?" ] && [ "$_strand" != "0" ] 2>/dev/null; then
        _count_line="$_count_line · $_strand stranded"
    fi
    if [ "${SP_NEXT_N}" -eq 0 ] 2>/dev/null; then
        printf ' %sNEXT%s   %s%s%s %s— nothing to claim%s\n' \
            "$C_DIM" "$C_RST" "$C_B" "$_count_line" "$C_RST" "$C_DIM" "$C_RST"
        return
    fi
    printf ' %sNEXT%s   %s%s%s %s— across all partitions:%s\n' "$C_DIM" "$C_RST" \
        "$C_B" "$_count_line" "$C_RST" "$C_DIM" "$C_RST"
    local i=0 raw
    while [ "$i" -lt "$MAX_NEXT_ROWS" ]; do
        eval "raw=\${SP_NEXT$i:-}"
        [ -n "$raw" ] || break
        next_row "$raw"
        i=$((i+1))
    done
}

# _q_hdr: emit ' QUEUE  ' on first call per section, '        ' thereafter.
# Uses _q_hdr_done from the calling queue_section() scope (bash dynamic scoping).
_q_hdr() {
    if [ "${_q_hdr_done:-0}" = 0 ]; then
        printf ' %sQUEUE%s  ' "$C_DIM" "$C_RST"
        _q_hdr_done=1
    else
        printf '        '
    fi
}

# QUEUE — done-to-landed pipeline funnel. One row per stage with count and oldest age.
# Ages go stale → actionable. A non-zero done with certified=0 means the gate is not
# working the queue (law-alerts-must-be-actionable).
queue_section() {
    local _bpr="${SP_QUEUE_BATCH_PR:-0}" _bn="${SP_QUEUE_BATCH_N:-0}"
    local _nn="${SP_QUEUE_NEXT_N:-0}" _uln="${SP_UNLANDED_N:-0}"
    local _nm="${SP_QUEUE_NEXT_MAX:-0}" _qqn="${SP_QUEUE_QUARANTINE_N:-0}"
    local _fn_certify="${SP_FUNNEL_CERTIFY_N:-?}" _fn_certify_age="${SP_FUNNEL_CERTIFY_AGE:-}"
    local _fn_red="${SP_FUNNEL_RED_N:-?}" _fn_red_age="${SP_FUNNEL_RED_AGE:-}"
    local _fn_red_to="${SP_FUNNEL_RED_TIMEOUT:-0}" _fn_red_rb="${SP_FUNNEL_RED_REBASE:-0}"
    local _fn_red_gt="${SP_FUNNEL_RED_GATE:-0}" _fn_red_cf="${SP_FUNNEL_RED_CONFLICT:-0}"
    local _fn_done_age="${SP_FUNNEL_DONE_AGE:-}"
    local _q_hdr_done=0

    if [ "${SP_QUEUE_DEPTH:-?}" = "?" ] && [ "$_bpr" = "0" ] && [ "$_nn" = "0" ] \
       && [ "$_uln" = "0" ] && [ "$_fn_certify" = "?" ] && [ "$_fn_red" = "?" ]; then
        unread_row QUEUE "cannot read the queue"
        return
    fi

    # --- Funnel rows: done → certify → red ---
    if [ "$_uln" != "0" ] || [ "$_uln" = "?" ]; then
        _q_hdr
        if [ "$_uln" = "?" ]; then
            printf '%sdone  ?%s\n' "$C_WARN" "$C_RST"
        else
            printf 'done  %s' "$_uln"
            [ -n "${_fn_done_age:-}" ] && printf '  oldest %s' "$_fn_done_age"
            printf '\n'
        fi
    fi
    if [ "$_fn_certify" != "0" ] || [ "$_fn_certify" = "?" ]; then
        _q_hdr
        if [ "$_fn_certify" = "?" ]; then
            printf '%scertify  ?%s\n' "$C_WARN" "$C_RST"
        else
            printf 'certify  %s' "$_fn_certify"
            [ -n "${_fn_certify_age:-}" ] && printf '  %s' "$_fn_certify_age"
            printf '\n'
        fi
    fi
    if [ "$_fn_red" != "0" ] || [ "$_fn_red" = "?" ]; then
        _q_hdr
        if [ "$_fn_red" = "?" ]; then
            printf '%sred  ?%s\n' "$C_WARN" "$C_RST"
        else
            printf '%sred  %s%s' "$C_WARN" "$_fn_red" "$C_RST"
            local _rd_br=""
            [ "$_fn_red_to" != "0" ] && _rd_br="${_rd_br}${_rd_br:+ · }${_fn_red_to} timeout"
            [ "$_fn_red_rb" != "0" ] && _rd_br="${_rd_br}${_rd_br:+ · }${_fn_red_rb} no-rebase"
            [ "$_fn_red_gt" != "0" ] && _rd_br="${_rd_br}${_rd_br:+ · }${_fn_red_gt} gate"
            [ "$_fn_red_cf" != "0" ] && _rd_br="${_rd_br}${_rd_br:+ · }${_fn_red_cf} conflict"
            [ -n "$_rd_br" ] && printf '  %s%s%s' "$C_DIM" "$_rd_br" "$C_RST"
            [ -n "${_fn_red_age:-}" ] && printf '  oldest %s' "$_fn_red_age"
            printf '\n'
        fi
    fi

    # --- Existing: active batch ---
    if [ "$_bpr" != "0" ] && [ "$_bpr" != "?" ]; then
        _q_hdr
        printf '%sbatch #%s%s %s(%s member(s))%s\n' \
            "$C_B" "$_bpr" "$C_RST" "$C_DIM" "$_bn" "$C_RST"
        local i=0 raw
        while [ "$i" -lt "$MAX_SECTION_ROWS" ]; do
            eval "raw=\${SP_QUEUE_BATCH$i:-}"
            [ -n "$raw" ] || break
            queue_row "$raw"
            i=$((i+1))
        done
    fi
    # --- Existing: certified next items ---
    if [ "$_nn" != "0" ] && [ "$_nn" != "?" ]; then
        if [ "$_bpr" != "0" ] && [ "$_bpr" != "?" ]; then
            printf '        %snext →%s\n' "$C_DIM" "$C_RST"
        else
            _q_hdr
            printf '%s%s certified%s\n' "$C_B" "$_nn" "$C_RST"
        fi
        local i=0 raw shown=0
        while [ "$i" -lt "$MAX_SECTION_ROWS" ]; do
            eval "raw=\${SP_QUEUE_NEXT$i:-}"
            [ -n "$raw" ] || break
            [ "$_nm" != "0" ] && [ "$shown" -eq "$_nm" ] && \
                printf '        %s· · ·%s\n' "$C_DIM" "$C_RST"
            queue_row "$raw"
            shown=$((shown+1))
            i=$((i+1))
        done
    fi
    if [ "$_q_hdr_done" = 0 ]; then
        _q_hdr
        printf '%snothing queued%s\n' "$C_DIM" "$C_RST"
    fi

    # Footer: ejected, quarantine (red is now a funnel row above)
    local _ej="${SP_QUEUE_EJECTED:-0}" _footer=""
    [ "$_ej" != "0" ] && [ "$_ej" != "?" ] && _footer="${_ej} ejected"
    [ "$_qqn" != "0" ] && [ "$_qqn" != "?" ] && _footer="${_footer}${_footer:+  }${_qqn} quarantined"
    [ -n "$_footer" ] && printf '        %s%s%s\n' "$C_WARN" "$_footer" "$C_RST"
}

# RECENT — what the harness did, newest first. The label shares the first row with the
# newest event rather than sitting on a line of its own: a section allocated one row must
# spend it on content, not on its own name.
recent_section() {
    local ev i
    eval "ev=\${SP_EVENT0:-}"
    if [ -z "$ev" ]; then
        # An empty log and an unread snapshot look identical from here, so they are
        # distinguished by whether the file exists at all rather than left as one dash.
        if [ -f "$SPIRA_SNAP" ]; then
            printf ' %sRECENT%s %snothing in the window%s\n' "$C_DIM" "$C_RST" "$C_DIM" "$C_RST"
        else
            unread_row RECENT "no snapshot to read"
        fi
        return
    fi
    # THE `\n` IS LOAD-BEARING, and it belongs to these format strings rather than to
    # recent_row: `$( )` strips trailing newlines, so the row's own one never survives being
    # captured. Without it every event was emitted onto one physical line — and because
    # `frame` sizes this section by the number of LINES it produced, the allocator was then
    # told RECENT wanted a single row and handed the rest of the column to NEXT. The section
    # rendering itself wrong and the column's arithmetic being wrong were one defect.
    printf ' %sRECENT%s %s\n' "$C_DIM" "$C_RST" "$(recent_row "$ev" 8)"
    # FROM ONE, BECAUSE THE HEADER ALREADY SPENT EVENT ZERO. The cap counts EVENTS, not the
    # rows beneath the label — five events is five events whether or not the newest of them
    # shares a line with the word RECENT.
    i=1
    while [ "$i" -lt "$MAX_RECENT_ROWS" ]; do
        eval "ev=\${SP_EVENT$i:-}"
        [ -n "$ev" ] || break
        printf '        %s\n' "$(recent_row "$ev" 8)"
        i=$((i+1))
    done
}

# INFLOW rows arrive as `<age> <kind> P<n> <bead> <title...>` and print in that order — the
# same left-to-right reading RECENT has (time, state, bead, description), so the two sections
# can be scanned down as one column rather than parsed separately.
#
# A ROW THAT CANNOT BE SPLIT IS PRINTED AS IT CAME, and still cut to the pane: autowrap is
# off, so an over-wide row is cut by the terminal instead, and the terminal's cut is silent.
inflow_row() {          # inflow_row "<age> <kind> P<n> <bead> <title>"
    local raw="$1" age kind pri id rest aw kw
    if [[ "$raw" =~ ^([0-9]+[smhd])[[:space:]]+([^[:space:]]+)[[:space:]]+(P[^[:space:]]+)[[:space:]]+([^[:space:]]+)[[:space:]]*(.*)$ ]]; then
        age="${BASH_REMATCH[1]}"; kind="${BASH_REMATCH[2]}"
        pri="${BASH_REMATCH[3]}"; id="${BASH_REMATCH[4]}"; rest="${BASH_REMATCH[5]}"
    else
        fit "$raw" $(( COLS - 8 ))
        printf '        %s%s%s\n' "$C_DIM" "$FIT" "$C_RST"; return
    fi
    # EIGHT COLUMNS OF KIND covers task, bug, epic, decision and incident — every type the
    # binary validates. A longer one widens its own row rather than being cut, as RECENT's
    # verb does: a type nothing here knows about is a fact worth seeing whole.
    aw=${#age};  [ "$aw" -lt 4 ] && aw=4
    kw=${#kind}; [ "$kw" -lt 8 ] && kw=8
    # fit width: 8 + aw+1 + kw+1 + |pri|+1 + max(14,id)+1 + fit = COLS
    fit "$rest" $(( COLS - 12 - aw - kw - ${#pri} - (${#id} > 14 ? ${#id} : 14) ))
    printf '        %s%-*s%s %s%-*s%s %s%s%s %s%-14s%s %s%s%s\n' \
        "$C_DIM" "$aw" "$age" "$C_RST" \
        "$(kind_colour "$kind")" "$kw" "$kind" "$C_RST" \
        "$(pri_colour "$pri")" "$pri" "$C_RST" \
        "$C_ACC" "$id" "$C_RST" "$C_DIM" "$FIT" "$C_RST"
}

# INFLOW — what is being CUT, newest first, and the rate it is arriving at.
#
# THE ONE SECTION THAT READS THE QUEUE FROM THE OTHER END. NOW says what is held, NEXT what
# is ready, UNLND what is landing and RECENT what moved; none of them answers where any of it
# came from. See the comment on MAX_INFLOW_ROWS for why that question is worth a section.
#
# THE HEADER IS A RATE AND THE ROWS ARE THE NEWEST, over two different windows on purpose. On
# a quiet hour the rate is 0 and the rows still say what last arrived and how long ago, so the
# section neither goes blank nor implies a burst that is not happening.
#
# THE KINDS CARRY THE COLOUR, NOT THE COUNT. Twelve new beads in an hour is a decomposed
# design or a loop, and the count alone cannot tell those apart — "bug 12" can
# (law-alerts-must-be-actionable: the figure that colours is the one a reader can act on).
inflow_section() {
    if [ -z "${SP_INFLOW_N:-}" ] || [ "${SP_INFLOW_N:-}" = "?" ]; then
        unread_row INFLOW "cannot read what is being cut"
        return
    fi
    local kcol="$C_DIM"
    case "${SP_INFLOW_DEFECT:-0}" in 0) ;; *) kcol="$C_WARN" ;; esac
    printf ' %sINFLOW%s %s%s%s %snew/%sm%s  %s%s%s\n' \
        "$C_DIM" "$C_RST" \
        "$C_B" "${SP_INFLOW_N}" "$C_RST" \
        "$C_DIM" "${SP_INFLOW_WIN:-?}" "$C_RST" \
        "$kcol" "${SP_INFLOW_KINDS:--}" "$C_RST"
    local i=0 raw
    while [ "$i" -lt "$MAX_INFLOW_ROWS" ]; do
        eval "raw=\${SP_INFLOW$i:-}"
        [ -n "$raw" ] || break
        inflow_row "$raw"
        i=$((i+1))
    done
}

# CI — work parked ON PURPOSE, and therefore invisible everywhere else on this pane: not
# in progress, not ready, nothing moving. The summary row answers "is anything parked";
# the rows beneath answer "which of them has been parked since yesterday", which is the
# question a run that never came back is found by.
#
# IT PRINTS EVEN AT ZERO. It used to print nothing at all when nothing was parked, so
# "nothing parked" and "this section is broken" were the same absence of pixels.
#
# TWO POPULATIONS, AND ONE LINE SAID BOTH. "Waiting on a run" is routine. "Parked with no
# run to wait for" is a bead that will wait forever: only a repository that lands through
# pull requests has a run at all, and the park label excludes a bead from every persona's
# predicate AND from the stranded-work report while it waits. So the second figure is a
# fault, is coloured like one, and is counted separately — a section reading "in CI" over a
# park nothing can end is the one description that stops the question being asked.
ci_section() {
    if [ -z "${SP_AWAITING_N:-}" ] || [ "${SP_AWAITING_N:-}" = "?" ]; then
        unread_row CI "cannot read what is parked on CI"
        return
    fi
    # BOTH COUNTS MUST BE ZERO for "nothing parked". Testing the first alone would report an
    # empty section over a queue of parks that no run will ever end, which is the failure this
    # section was split to make impossible.
    if [ "${SP_AWAITING_N}" -eq 0 ] 2>/dev/null && [ "${SP_AWAITING_STUCK:-0}" -eq 0 ] 2>/dev/null; then
        printf ' %sCI%s     %snothing parked on CI%s\n' "$C_DIM" "$C_RST" "$C_DIM" "$C_RST"
        return
    fi
    local ci_col="$C_DIM" stuck_txt=""
    case "${SP_AWAITING_AGE:-}" in *h|*d) ci_col="$C_WARN" ;; esac
    if [ "${SP_AWAITING_STUCK:-0}" != 0 ]; then
        stuck_txt="$(printf '   %s%s parked with no run to wait for%s %s(%s)%s' \
            "$C_WARN" "${SP_AWAITING_STUCK}" "$C_RST" \
            "$C_DIM" "${SP_AWAITING_STUCK_ID:-?}" "$C_RST")"
    fi
    printf ' %sCI%s     %s%s bead(s) waiting on a run%s   %soldest%s %s %s(%s)%s%s\n' \
        "$C_DIM" "$C_RST" "$C_B" "${SP_AWAITING_N}" "$C_RST" \
        "$C_DIM" "$C_RST" "${SP_AWAITING_OLDEST:-?}" \
        "$ci_col" "${SP_AWAITING_AGE:-?}" "$C_RST" "$stuck_txt"
    local i=0 row
    while [ "$i" -lt "$MAX_SECTION_ROWS" ]; do
        eval "row=\${SP_AWAITING$i:-}"
        [ -n "$row" ] || break
        fit "$row" $(( COLS - 8 ))
        printf '        %s%s%s\n' "$C_DIM" "$FIT" "$C_RST"
        i=$((i+1))
    done
}

# The standing figures. Fixed height by construction — each is one number that is always
# worth a row — so they are not part of the share and are never elided by it.
standing_lines() {
    # FLOW — everything moving between the operator and the harness, in one place.
    local unans_col="$C_OK"; [ "${SP_UNANSWERED:-0}" != 0 ] && unans_col="${C_BAD}${C_B}"
    # ALARM DIMS WHEN OLD. The 24h FAILED count is a monument to every refusal in the window,
    # not a description of what is happening now. A count that is non-zero but whose most
    # recent FAILED line is older than 60m (30× the 2m sending interval) is resolved — the
    # condition stopped hours ago and the counter will decay to 0 on its own. Bright red for
    # an active or recent condition; dim once it is clearly past.
    local fail_col="$C_OK"
    if [ "${SP_SENT_FAILED:-0}" != 0 ]; then
        if [ "${SP_SENT_FAILED_AGE_M:-?}" != "?" ] && [ "${SP_SENT_FAILED_AGE_M:-0}" -ge 60 ] 2>/dev/null; then
            fail_col="$C_DIM"
        else
            fail_col="${C_BAD}${C_B}"
        fi
    fi
    # TWO LINES. As one it ran to ~140 characters against a pane about 100 wide and
    # truncated mid-word — which is how a dashboard ends up showing a stray "f".
    printf ' %sATTN%s   %swaiting on you%s %s%s%s   %sthreads awaiting my reply%s %s%s%s\n' \
        "$C_DIM" "$C_RST" \
        "$C_DIM" "$C_RST" "$C_B" "${SP_WAITING:-?}" "$C_RST" \
        "$C_DIM" "$C_RST" "$unans_col" "${SP_UNANSWERED:-?}" "$C_RST"
    # WHERE THIS SITS IN THE LIFECYCLE, because it was read as the verification BEFORE a
    # bead closes and it is the opposite end: gate.sh is the trial a branch passes before
    # it may merge, CHECK 6 is the merge, and the Sending is what happens AFTER — laying
    # the landed branch and its worktree to rest so they do not come back.
    #
    # NAME THE NOUN. "6 unsent" never said unsent WHAT — branches, aeons, stories, epics,
    # PRs were all plausible readings of the same number. It is BRANCHES, each with a
    # worktree, and saying so costs one word.
    #
    # THE METAPHOR STAYS; IT CARRIES ITS OWN GLOSS. (the operator, verbatim: "i like insider
    # vocab from FFX i just don't know what it means".) A Sending is the rite that lays the
    # dead to rest so they do not become fiends — which is exactly what reaping a landed
    # branch does, and exactly what happens when it fails: an unsent branch came back and
    # re-landed itself every two minutes for twenty minutes. So the words stay and each one
    # is followed by what it means in this system.
    #
    # What changed is the FIGURES. "reaped 19 · held 24 · awaiting land 13" was 24h of the
    # reaper's history; none of it said whether anything needs attention. These say what is
    # on the shelf right now.
    local age_col="$C_DIM"
    [ "${SP_UNSENT_OLDEST_H:-0}" = "?" ] || { [ "${SP_UNSENT_OLDEST_H:-0}" -ge 24 ] 2>/dev/null && age_col="$C_WARN"; }
    # TWO LINES, FOR THE THIRD TIME AND THE SAME REASON. At ~98 characters this ran off a
    # column a third of a 214-wide window, and the terminal cuts rather than folds — so the
    # gloss that makes "fiends" mean anything was the part that disappeared. The counts stay
    # on the first row; the word that needs explaining takes the second.
    local unadopt_seg=""
    [ "${SP_UNADOPTED:-0}" != 0 ] && unadopt_seg="$(printf ' · %s%s unadopted%s' "$C_WARN" "${SP_UNADOPTED}" "$C_RST")"
    printf ' %sSEND%s   %s%s branches unsent%s · %s%s awaiting rites%s · %soldest %sh%s%s\n' \
        "$C_DIM" "$C_RST" "$C_B" "${SP_UNSENT:-?}" "$C_RST" \
        "$( [ "${SP_BRANCH_DONE:-0}" = 0 ] && printf '%s' "$C_DIM" || printf '%s' "$C_WARN")" "${SP_BRANCH_DONE:-?}" "$C_RST" \
        "$age_col" "${SP_UNSENT_OLDEST_H:-?}" "$C_RST" "$unadopt_seg"
    local fiend_age_sfx=""
    if [ "${SP_SENT_FAILED:-0}" != 0 ]; then
        if [ "${SP_SENT_FAILED_AGE_M:-?}" = "?" ]; then
            fiend_age_sfx=" ${C_DIM}(last: ?)${C_RST}"
        else
            fiend_age_sfx=" ${C_DIM}(last: ${SP_SENT_FAILED_AGE_M}m ago)${C_RST}"
        fi
    fi
    printf '        %s%s fiends%s %s— unsent work that came back%s%s\n' \
        "$fail_col" "${SP_SENT_FAILED:-?}" "$C_RST" "$C_DIM" "$C_RST" "$fiend_age_sfx"

    # 24H — throughput, and whether closing meant landing.
    printf ' %sBEADS%s  %s24h%s  closed %s · %sopened%s %s\n' \
        "$C_DIM" "$C_RST" "$C_DIM" "$C_RST" "${SP_CLOSED_24H:-?}" \
        "$C_DIM" "$C_RST" "${SP_OPENED_24H:-?}"
    # Sparklines: opened and closed share a row (they are a flow balance); landed is its
    # own row (bursty, different scale). Each series is scaled to itself — the question is
    # "rising or falling" for that series, not a cross-series comparison. A `?` from the
    # probe means the source could not be read (law-absence-needs-a-positive-control).
    printf '        %sopened%s %s  %sclosed%s %s\n' \
        "$C_DIM" "$C_RST" "${SP_BEADS_SPARK_OPENED:-?}" \
        "$C_DIM" "$C_RST" "${SP_BEADS_SPARK_CLOSED:-?}"
    printf '        %slanded%s %s  %s%s%s in 24h\n' \
        "$C_DIM" "$C_RST" "${SP_BEADS_SPARK_LANDED:-?}" \
        "$C_DIM" "${SP_BEADS_LANDED_24H:-?}" "$C_RST"
    # The kinds are a breakdown of the number above, so they read as one when they sit under
    # it — and on their own row they can be four kinds rather than however many fit.
    fit "${SP_CLOSED_KINDS:--}" $(( COLS - 8 ))
    printf '        %s%s%s\n' "$C_DIM" "$FIT" "$C_RST"
    # THE ROW STATES ITS OWN WINDOW. Previously it inherited "24h" from the header while its
    # population was all-time, so comparing it against the 24h closed/opened counts above
    # produced nonsense — 59% of the row was outside its header's window.
    printf '        %s24h worked %s:%s %s landed · %s%s queued%s · %s%s done%s\n' \
        "$C_DIM" "${SP_CLOSED:-?}" "$C_RST" "${SP_LANDED:-?}" \
        "$C_DIM" "${SP_QUEUE_DEPTH:-?}" "$C_RST" \
        "$( [ "${SP_UNLANDED_N:-0}" = 0 ] && printf '%s' "$C_OK" || printf '%s' "$C_BAD$C_B")" "${SP_UNLANDED_N:-?}" "$C_RST"
    fit "done = closed, has branch, no landstate" $(( COLS - 8 ))
    printf '        %s%s%s\n' "$C_DIM" "$FIT" "$C_RST"

    # ACCEPTANCE — the only row that says whether a release INSTALLS, not whether its gate
    # was green. The operator (2026-09-23): "i still don't know what the fuck is going on
    # with acceptance because i have no pane visibility about that."
    #
    # TWO FACTS, DELIBERATELY. The verdict alone is a lie by omission: on the day this was
    # written the newest recorded verdict was three days and four releases old, so a row
    # printing "FAIL" or "PASS" on its own would have described a tag nobody was shipping.
    # "since" is how many release tags have been cut with no verdict at all, and it is the
    # number that turns a stale green into an obvious hole.
    #
    # NEVER and ? ARE NOT FAIL. Never recorded, and could-not-read, are different answers
    # and both are different from a recorded failure. A row that renders 0 or FAIL for a
    # probe it could not run is the failure mode this whole pane exists to avoid.
    local acc_col acc_age
    case "${SP_ACCEPT_VERDICT:-?}" in
        PASS)  acc_col="$C_OK" ;;
        FAIL)  acc_col="$C_BAD$C_B" ;;
        NEVER) acc_col="$C_WARN" ;;
        *)     acc_col="$C_WARN" ;;
    esac
    acc_age="?"
    if [ -n "${SP_ACCEPT_AT:-}" ] && [ "${SP_ACCEPT_AT:-?}" != "?" ]; then
        acc_age="$(age_str $(( $(date +%s) - SP_ACCEPT_AT )) 86400)"
    fi
    printf '        %saccept%s %s%s%s %s · %s%s untested since%s\n' \
        "$C_DIM" "$C_RST" \
        "$acc_col" "${SP_ACCEPT_VERDICT:-?}" "$C_RST" "$acc_age" \
        "$( [ "${SP_ACCEPT_SINCE:-1}" = 0 ] && printf '%s' "$C_OK" || printf '%s' "$C_BAD$C_B")" \
        "${SP_ACCEPT_SINCE:-?}" "$C_RST"
    fit "${SP_ACCEPT_TAG:--}" $(( COLS - 8 ))
    printf '        %s%s%s\n' "$C_DIM" "$FIT" "$C_RST"

    # LAND — the DONE-to-LANDED stretch. The operator (2026-09-07): "there's currently a
    # lot that happens between 'DONE' and 'LANDED' and the ops dashboard shows none of it."
    #
    # The landing pass's own state renders from landing.status without the collector
    # recomputing any of it; live gate runs render with their branch, age, and whether they
    # are waiting on the tree lock or running suites.
    local land_age="?"
    if [ -n "${SP_LAND_AT:-}" ] && [ "${SP_LAND_AT:-?}" != "?" ]; then
        land_age=$(( $(date +%s) - SP_LAND_AT ))
    fi
    local land_rc_col
    case "${SP_LAND_RC:-?}" in
        0) land_rc_col="$C_OK" ;;
        '?') land_rc_col="$C_BAD$C_B" ;;
        *) land_rc_col="$C_WARN" ;;
    esac
    printf ' %sLAND%s   %slast%s %s  %src%s %s%s%s  %sbranches%s %s  %smoved%s %s\n' \
        "$C_DIM" "$C_RST" \
        "$C_DIM" "$C_RST" "$(age_str "$land_age" 600)" \
        "$C_DIM" "$C_RST" "$land_rc_col" "${SP_LAND_RC:-?}" "$C_RST" \
        "$C_DIM" "$C_RST" "${SP_LAND_BRANCHES:-?}" \
        "$C_DIM" "$C_RST" "${SP_LAND_MOVED:-?}"
    if [ "${SP_GATE_LIVE:-0}" != "0" ] && [ "${SP_GATE_LIVE:-0}" != "?" ]; then
        printf '        %s%s gate(s) running:%s\n' "$C_B" "${SP_GATE_LIVE}" "$C_RST"
        local gi=0
        while [ "$gi" -lt "${SP_GATE_N:-0}" ] 2>/dev/null; do
            eval "local gs=\${SP_GATE${gi}_SLUG:-?} ga=\${SP_GATE${gi}_AGE:-?}"
            eval "local gp=\${SP_GATE${gi}_PHASE:-?} gw=\${SP_GATE${gi}_WHY:-}"
            local age_s
            if [ "$ga" = "?" ]; then age_s="?"
            elif [ "$ga" -lt 120 ] 2>/dev/null; then age_s="${ga}s"
            else age_s="$(( ga / 60 ))m"; fi
            local phase_col="$C_DIM"
            [ "$gp" = waiting ] && phase_col="$C_WARN"
            fit "${gw:+ · $gw}" $(( COLS - 30 - ${#gs} - ${#age_s} ))
            printf '        %s%s%s  %s%-7s%s  %s%s%s%s\n' \
                "$C_ACC" "$gs" "$C_RST" \
                "$phase_col" "$gp" "$C_RST" \
                "$C_DIM" "$age_s" "$C_RST" \
                "$( [ -n "${gw:-}" ] && printf ' %s%s%s' "$C_DIM" "$FIT" "$C_RST" )"
            gi=$((gi+1))
        done
    elif [ "${SP_GATE_LIVE:-?}" = "?" ]; then
        printf '        %s%s? cannot read gate state%s\n' "$C_BAD" "$C_B" "$C_RST"
    fi
    # Landing progress — per-branch outcomes of the pass in flight.
    if [ "${SP_LANDPROG_N:-0}" != "0" ] && [ "${SP_LANDPROG_N:-0}" != "?" ]; then
        local li=0
        while [ "$li" -lt "${SP_LANDPROG_N:-0}" ] 2>/dev/null; do
            eval "local lp=\${SP_LANDPROG${li}:-}"
            if [ -n "$lp" ]; then
                fit "$lp" $(( COLS - 10 ))
                printf '        %s%s%s\n' "$C_DIM" "$FIT" "$C_RST"
            fi
            li=$((li+1))
        done
    fi

    # GRAPH — the beads themselves, kept apart from sessions and asks so the counts cannot
    # be read as the same kind of thing.
    # STRANDED IS THE GHOST COUNT, NOT THE LEDGER SIZE. strands.json holds every disposition
    # strand.sh classifies and only `ghost` is a claimed bead whose holder is gone; the size
    # rendered under that name made a childless epic read as a dead worker. The ledger total
    # stays alongside in parentheses, because it is context rather than an alarm.
    local _graph
    _graph="$(printf 'open %s · ready %s · working %s · poison %s · strand %s (ledger %s)' \
        "${SP_OPEN:-?}" "${SP_READY:-?}" "${SP_INPROG:-?}" "${SP_POISON:-?}" \
        "${SP_STRAND_GHOST:-?}" "${SP_STRANDS:-?}")"
    fit "$_graph" $(( COLS - 8 ))
    printf '        %s%s%s\n' \
        "$( [ "${SP_POISON:-0}" = 0 ] && printf '%s' "$C_DIM" || printf '%s' "$C_WARN")" "$FIT" "$C_RST"

    # LIVELOCK — open beads that cannot make progress, and closed beads with bad records.
    # The count alone is the line; individual bead rows live in SP_LIVELOCK0..N-1,
    # SP_INVCLSD0..N-1 and SP_UNFLFLW0..N-1 (rendered only when non-zero).
    # SP_LIVELOCKED=? means the probe failed; that is not all-clear.
    local _ll="${SP_LIVELOCKED:-?}" _ic="${SP_INVALID_CLOSED:-?}" _uf="${SP_UNFILED_FOLLOW:-?}"
    local _ll_col _ic_col _uf_col
    case "$_ll" in
        0)   _ll_col="$C_DIM" ;;
        '?') _ll_col="$C_BAD$C_B" ;;
        *)   _ll_col="$C_WARN$C_B" ;;
    esac
    case "$_ic" in
        0)   _ic_col="$C_DIM" ;;
        '?') _ic_col="$C_BAD$C_B" ;;
        *)   _ic_col="$C_WARN$C_B" ;;
    esac
    case "$_uf" in
        0)   _uf_col="$C_DIM" ;;
        '?') _uf_col="$C_BAD$C_B" ;;
        *)   _uf_col="$C_WARN$C_B" ;;
    esac
    if [ "$_ll" != 0 ] || [ "$_ic" != 0 ] || [ "$_uf" != 0 ]; then
        printf ' %sLOCK%s   %slivelocked%s %s%s%s · %sinvalid-closed%s %s%s%s · %sunfiled-follow%s %s%s%s\n' \
            "$C_DIM" "$C_RST" \
            "$C_DIM" "$C_RST" "$_ll_col" "$_ll" "$C_RST" \
            "$C_DIM" "$C_RST" "$_ic_col" "$_ic" "$C_RST" \
            "$C_DIM" "$C_RST" "$_uf_col" "$_uf" "$C_RST"
        local _ll_i=0 _ic_i=0 _uf_i=0
        while [ "$_ll_i" -lt "${SP_LIVELOCK_N:-0}" ] 2>/dev/null && [ "$_ll_i" -lt 5 ]; do
            eval "local _ll_row=\${SP_LIVELOCK${_ll_i}:-}"
            if [ -n "$_ll_row" ]; then
                fit "$_ll_row" $(( COLS - 12 ))
                printf '        %s  %s%s%s\n' "$C_DIM" "$C_WARN" "$FIT" "$C_RST"
            fi
            _ll_i=$(( _ll_i + 1 ))
        done
        while [ "$_ic_i" -lt "${SP_INVCLSD_N:-0}" ] 2>/dev/null && [ "$_ic_i" -lt 5 ]; do
            eval "local _ic_row=\${SP_INVCLSD${_ic_i}:-}"
            if [ -n "$_ic_row" ]; then
                fit "$_ic_row" $(( COLS - 12 ))
                printf '        %s  %s%s%s\n' "$C_DIM" "$C_WARN" "$FIT" "$C_RST"
            fi
            _ic_i=$(( _ic_i + 1 ))
        done
        while [ "$_uf_i" -lt "${SP_UNFLFLW_N:-0}" ] 2>/dev/null && [ "$_uf_i" -lt 5 ]; do
            eval "local _uf_row=\${SP_UNFLFLW${_uf_i}:-}"
            if [ -n "$_uf_row" ]; then
                fit "$_uf_row" $(( COLS - 12 ))
                printf '        %s  %s%s%s\n' "$C_DIM" "$C_WARN" "$FIT" "$C_RST"
            fi
            _uf_i=$(( _uf_i + 1 ))
        done
    fi

    # GATE — is the check between work and its landings buying anything? The pane already
    # instruments what the harness COSTS; this is the only line that says whether one of its
    # costs is earning its place (law-gate-earns-its-place). It is here because the previous
    # gate was deleted after twelve hours of fallout rather than on evidence, and every fact
    # needed to delete it a fortnight earlier was already being written down.
    #
    # UNKNOWN GETS ITS OWN FIGURE AND IS NEVER FOLDED INTO EITHER. A yield of "3 defect, 8
    # fault" reads as a complete accounting; if half the reds were never classified, the
    # ratio is a fiction and only a visible unknown count says so.
    #
    # SOLO AND CONTENDED, BOTH, because a landing gate never runs solo — every aeon runs one
    # before it closes and the landing pass runs one per branch — so a solo figure describes
    # a condition the gate is never in. The gap between the two is what decides whether it is
    # affordable, and quoting only the first is how one was adopted at 101s and turned out to
    # cost 329s.
    local fault_col
    case "${SP_YIELD_FAULT:-?}" in
        0)  fault_col="$C_OK" ;;
        '?') fault_col="$C_BAD$C_B" ;;   # unreadable is a fault of its own, not all-clear
        *)  fault_col="$C_WARN" ;;
    esac
    # A UNIT ON AN UNREADABLE FIELD INVITES READING IT AS A MEASUREMENT: `?s` looks like a
    # duration somebody forgot to fill in, and a `?` here means this probe could not read the
    # gate log at all.
    local solo="${SP_YIELD_SOLO_MED:-?}" conc="${SP_YIELD_CONC_MED:-?}"
    [ "$solo" = "?" ] || solo="${solo}s"
    [ "$conc" = "?" ] || conc="${conc}s"
    printf ' %sGATE%s   %sreds%s %s · %sdefect%s %s · %sgate fault%s %s%s%s · %sunknown%s %s\n' \
        "$C_DIM" "$C_RST" \
        "$C_DIM" "$C_RST" "${SP_YIELD_REDS:-?}" \
        "$C_DIM" "$C_RST" "${SP_YIELD_DEFECT:-?}" \
        "$C_DIM" "$C_RST" "$fault_col" "${SP_YIELD_FAULT:-?}" "$C_RST" \
        "$C_DIM" "$C_RST" "${SP_YIELD_UNKNOWN:-?}"
    printf '        %scost%s %s solo · %s with another gate overlapping %s(median)%s\n' \
        "$C_DIM" "$C_RST" "$solo" "$conc" "$C_DIM" "$C_RST"

    local _st_sum="${SP_SUITE_LAST_SUM:-?}" _st_wall="${SP_SUITE_LAST_WALL:-?}"
    [ "$_st_wall" = "?" ] || _st_wall="${_st_wall}s"
    [ "$_st_sum"  = "?" ] || _st_sum="${_st_sum}s"
    printf ' %sSUITES%s %ssum%s %s  %swall%s %s\n' \
        "$C_DIM" "$C_RST" \
        "$C_DIM" "$C_RST" "$_st_sum" \
        "$C_DIM" "$C_RST" "$_st_wall"

    # MAIL — concierge mailbox: unread count and per-message state.
    _mail_dur() {   # <seconds> <varname> — human age: 90s → 1m, 3661s → 1h, 90000s → 1d
        local _s="$1" _v="$2"
        case "$_s" in ''|'?'|'-'|*[!0-9]*) printf -v "$_v" '%s' "${_s:-?}"; return ;; esac
        if   [ "$_s" -lt 120 ];   then printf -v "$_v" '%ds' "$_s"
        elif [ "$_s" -lt 7200 ];  then printf -v "$_v" '%dm' $(( _s / 60 ))
        elif [ "$_s" -lt 86400 ]; then printf -v "$_v" '%dh' $(( _s / 3600 ))
        else                           printf -v "$_v" '%dd' $(( _s / 86400 )); fi
    }
    local m_unread="${SP_MAIL_UNREAD:-?}" m_oldest="${SP_MAIL_OLDEST_AGE:--}"
    if [ "$m_unread" = "?" ]; then
        printf ' %sMAIL%s   %s? cannot read mailbox%s\n' "$C_DIM" "$C_RST" "$C_BAD$C_B" "$C_RST"
    else
        local m_age_sfx="" m_dur_tmp
        if [ "$m_oldest" != "-" ] && [ "${m_unread:-0}" != 0 ] 2>/dev/null; then
            _mail_dur "$m_oldest" m_dur_tmp
            m_age_sfx="  ${C_DIM}oldest ${m_dur_tmp}${C_RST}"
        fi
        local m_col; [ "${m_unread:-0}" = 0 ] && m_col="$C_DIM" || m_col="${C_WARN}${C_B}"
        printf ' %sMAIL%s   %s%s unread%s%s\n' \
            "$C_DIM" "$C_RST" "$m_col" "$m_unread" "$C_RST" "$m_age_sfx"
        local mi=0 mn="${SP_MAIL_N:-0}"
        while [ "$mi" -lt "${mn:-0}" ] 2>/dev/null && [ "$mi" -lt 5 ]; do
            eval "local mrow=\${SP_MAIL${mi}:-}"
            if [ -n "$mrow" ]; then
                local m_age_raw m_state m_subj m_dur
                IFS='|' read -r m_age_raw m_state m_subj <<< "$mrow"
                _mail_dur "${m_age_raw:-0}" m_dur
                local st_col
                case "$m_state" in
                    NEW)  st_col="${C_WARN}${C_B}" ;;
                    READ) st_col="$C_ACC"          ;;
                    DONE) st_col="$C_DIM"          ;;
                    *)    st_col="$C_BAD"          ;;
                esac
                fit "${m_subj:-}" $(( COLS - 22 ))
                printf '        %s%4s%s  %s%-4s%s  %s%s%s\n' \
                    "$C_DIM" "$m_dur" "$C_RST" \
                    "$st_col" "$m_state" "$C_RST" \
                    "$C_DIM" "$FIT" "$C_RST"
            fi
            mi=$(( mi + 1 ))
        done
    fi

    # OPS — the SOP shelf. Which runbooks have never been exercised (dead weight in context),
    # which have been applied but did not hold within the window, and how long ago the last
    # sweep pass ran. `?` for any field means the shelf, ledger, or read failed; see sop_keys()
    # in cockpit.sh. SP_SWEEP_AGE=? distinguishes "sweep never ran" from "ran and found nothing"
    # (law-arm-before-you-retire): a quiet sweep and a stopped one are the same pixels without it.
    printf ' %sOPS%s    %snever-fired%s %s · %srecurred (no hold)%s %s · %ssweep-last%s %s\n' \
        "$C_DIM" "$C_RST" \
        "$C_DIM" "$C_RST" "$(num "${SP_SOP_NEVER_FIRED:-?}" 5)" \
        "$C_DIM" "$C_RST" "$(bad_unless_zero "${SP_SOP_RECURRED:-?}")" \
        "$C_DIM" "$C_RST" "$(age_str "${SP_SWEEP_AGE:-?}" 1800)"
}

# share <rows> <fixed> <base:max:first...> -> one allocation per section, on its own line
#
# ROUND-ROBIN, ONE ROW AT A TIME. Filling each section to its cap in turn is the obvious
# implementation and the wrong one: NOW spends three rows per working aeon, so on a busy
# day it would take the whole column and CI — the section that reports a run parked since
# yesterday, and the only place that fact appears — would be the one to vanish.
#
# THREE TIERS, BECAUSE TWO ROUND-ROBINS CANNOT SAY ALL THREE THINGS. Every section names a
# BASE it needs, a MAX it could use, and whether it is served FIRST. The first-served
# sections — NOW, UNLANDED, CI — fill to their max before any other section reaches its
# base: this is the guarantee that NOW fills before the three list sections compete for
# budget. Without tier 0, a fair round-robin over all sections handed NOW only 9 of its 12
# rows on a 45-row pane while NEXT and RECENT each held five — the section describing work
# in flight trimmed to feed the ones that fill space. Tier one then fills every remaining
# section to its base; tier two is the slack, where only a section with room above its base
# bids (INFLOW).
#
# FIRST-SERVED IS DECLARED, NOT INFERRED FROM base == max. It was inferred, and the day NEXT
# and RECENT stopped being elastic they silently joined tier 0 and drank the budget ahead of
# INFLOW's floor — a section that had just been given a five-row guarantee rendered one row
# on a three-aeon pane. Nothing in the spec had said those two were first-served; the
# equality that made them so was a side effect of capping them. A section's PLACE IN THE
# QUEUE and its SIZE are two facts, and a shape that derives one from the other cannot be
# changed without changing both.
#
# EVERY SECTION KEEPS ITS FIRST ROW even when there is no budget for it. The overflow then
# falls off the BOTTOM of the frame in `render`, which marks the count on the header; a
# section quietly allocated zero rows would leave no such mark.
share() {
    local rows="$1" fixed="$2"; shift 2
    local -a spec=("$@") base=() max=() first=() give=()
    local n=${#spec[@]} i budget moved tier lim
    for (( i = 0; i < n; i++ )); do
        IFS=: read -r base[i] max[i] first[i] <<< "${spec[i]}"
        # A MAX BELOW ITS BASE IS THE BASE. The two are computed independently at the call
        # site — one from a constant, one from how many rows the section actually rendered —
        # and a section with three rows of data must not be asked for five.
        [ "${max[i]}" -lt "${base[i]}" ] 2>/dev/null && max[i]="${base[i]}"
        if [ "${max[i]}" -gt 0 ]; then give[i]=1; else give[i]=0; fi
    done
    # 0 rows means no limit, which is what `once` uses: give every section everything.
    [ "$rows" -le 0 ] && { printf '%s\n' "${max[@]}"; return; }
    budget=$(( rows - fixed ))
    for (( i = 0; i < n; i++ )); do budget=$(( budget - give[i] )); done
    for tier in 0 1 2; do
        moved=1
        while [ "$budget" -gt 0 ] && [ "$moved" = 1 ]; do
            moved=0
            for (( i = 0; i < n; i++ )); do
                [ "$budget" -gt 0 ] || break
                if [ "$tier" = 0 ]; then
                    [ "${first[i]}" = 1 ] || continue
                    lim="${max[i]}"
                elif [ "$tier" = 1 ]; then lim="${base[i]}"
                else lim="${max[i]}"; fi
                if [ "${give[i]}" -lt "$lim" ]; then
                    give[i]=$(( give[i] + 1 )); budget=$(( budget - 1 )); moved=1
                fi
            done
        done
    done
    printf '%s\n' "${give[@]}"
}

# The predecessor section was removed when that harness was retired: a dashboard
# that keeps reporting a system nobody runs trains the eye to skip the pane. Its figures —
# mayor/deacon dots, merge-queue depth, parked beads, orphans, dispatch mode — described
# agents, a scheduler and a queue that no longer exist. Disk stayed, in standing_lines.
#
# frame <rows> <cols> — assemble one frame for a pane of this size (0 rows = unlimited).
#
# The five elastic sections are rendered IN FULL first and then cut to their allocation.
# Building them first is what makes the share honest: the allocator is told what each
# section could actually use, rather than a guess made before the data was read.
frame() {
    local rows="${1:-0}"
    # A GLOBAL, DELIBERATELY. Every section needs the width and none of them takes an
    # argument for it — they are called through process substitution and would have to
    # thread it through unchanged. Set once per frame, so a resize reflows on the next tick.
    COLS="${2:-0}"; [ "$COLS" -gt 0 ] 2>/dev/null || COLS=80
    load_snapshot
    local -a HEAD TOKENS NOW NEXT UNLANDED RECENT INFLOW CI STANDING give
    mapfile -t HEAD     < <(header_line)
    mapfile -t TOKENS   < <(tokens_section)
    mapfile -t NOW      < <(now_section)
    mapfile -t NEXT     < <(next_section)
    mapfile -t UNLANDED < <(queue_section)
    mapfile -t RECENT   < <(recent_section)
    mapfile -t INFLOW   < <(inflow_section)
    mapfile -t CI       < <(ci_section)
    mapfile -t STANDING < <(standing_lines)

    # THE GENERIC CAP APPLIES TO CI AND TO NOTHING ELSE NOW.
    #
    # NEXT and RECENT bound themselves at MAX_NEXT_ROWS and MAX_RECENT_ROWS while rendering,
    # so a second cap over them would only ever be dead code that looked load-bearing.
    #
    # AND NOW IS DELIBERATELY UNCAPPED HERE. Its want is four rows per live aeon, which is
    # already its own bound — a figure set by how many sessions are running, not by a number
    # written down here. Holding it to twenty as well would mean the section that just got
    # four rows deep is the first one the allocator trims, which is exactly backwards: it is
    # the section describing work in flight.
    #
    # NOW IS ALSO WHY ONE SECTION MUST STAY ELASTIC. NOW's want is the only one on this pane
    # that swings with the state of the world, so it is the one the column has to absorb:
    # every row it takes when three aeons wake up has to come from somewhere, and every row it
    # gives back when they finish has to go somewhere. With every section pinned, the
    # giving-back had nowhere to go and the bottom of the pane simply went blank. INFLOW is
    # now the section that takes it — see MAX_INFLOW_ROWS for why it is that one and not NEXT
    # or RECENT, whose seventh and eighth rows nobody reads.
    #
    # CI and UNLANDED keep the generic cap because they are lists rather than a glance, and
    # an unbounded one could otherwise take the whole column.
    #
    # EACH SECTION BIDS A BASE AND A MAX. All but INFLOW bid the same for both: their size is
    # set by how many aeons are awake, how much is pending or parked, and two constants that
    # are now heights rather than ceilings — so there is no slack in them to give away and
    # nothing to gain by asking for more than they have. INFLOW bids the five it is guaranteed
    # and everything it rendered, and is therefore the only bidder in tier two.
    #
    # INDICES ARE POSITIONAL: NOW=0, NEXT=1, UNLANDED=2, RECENT=3, INFLOW=4, CI=5. Adding a
    # section in the middle shifts every index after it; they must be updated together.
    local -a want=("${#NOW[@]}" "${#NEXT[@]}" "${#UNLANDED[@]}" "${#RECENT[@]}" \
                   "${#INFLOW[@]}" "${#CI[@]}")
    [ "${want[2]}" -gt "$MAX_SECTION_ROWS" ] && want[2]="$MAX_SECTION_ROWS"
    [ "${want[5]}" -gt "$MAX_SECTION_ROWS" ] && want[5]="$MAX_SECTION_ROWS"
    # THE BASES ARE IN LINES, NOT ITEMS, because that is what the allocator hands out. NEXT
    # and INFLOW each spend a line of their own on a header, so five rows is six lines; RECENT
    # puts the newest event on its header line, so five events is five. Getting this wrong
    # would quietly move one of them off the guarantee by a row.
    #
    # AND A BASE IS CAPPED BY WHAT THE SECTION ACTUALLY RENDERED. A base above the want is a
    # section handed rows it has no content for: the allocator charges them to the budget,
    # the slice prints what exists, and the difference is blank lines at the bottom of the
    # column — the same waste this file already fixed once, arriving by way of a quiet queue
    # instead of a tall pane.
    local nb=$(( NEXT_BASE_ROWS + 1 )) rb="$RECENT_BASE_ROWS" ib=$(( INFLOW_BASE_ROWS + 1 ))
    [ "${want[1]}" -lt "$nb" ] && nb="${want[1]}"
    [ "${want[3]}" -lt "$rb" ] && rb="${want[3]}"
    [ "${want[4]}" -lt "$ib" ] && ib="${want[4]}"
    #
    # THE THIRD FIELD IS "SERVED FIRST", and only the three sections whose height is set by
    # the state of the world carry it: NOW by how many aeons are awake, UNLND by how much is
    # waiting to land, CI by how much is parked. NEXT, RECENT and INFLOW are lists of a fixed
    # shape, and they degrade TOGETHER in tier one rather than in the order they happen to be
    # written here — which is the whole reason INFLOW keeps a floor on a three-aeon pane.
    local -a spec=(
        "${want[0]}:${want[0]}:1"
        "$nb:${want[1]}:0"
        "${want[2]}:${want[2]}:1"
        "$rb:${want[3]}:0"
        "$ib:${want[4]}:0"
        "${want[5]}:${want[5]}:1"
    )
    # TOKENS counts as FIXED, alongside the header and the standing figures: every one of its
    # rows is a number that is always worth its row, and the constraint that stops all other
    # work must not be what the allocator elides on a short pane.
    mapfile -t give < <(share "$rows" \
        $(( ${#HEAD[@]} + ${#TOKENS[@]} + ${#STANDING[@]} )) "${spec[@]}")

    printf '%s\n' "${HEAD[@]}"
    printf '%s\n' "${TOKENS[@]}"
    [ "${give[0]}" -gt 0 ] && printf '%s\n' "${NOW[@]:0:${give[0]}}"
    [ "${give[1]}" -gt 0 ] && printf '%s\n' "${NEXT[@]:0:${give[1]}}"
    [ "${give[2]}" -gt 0 ] && printf '%s\n' "${UNLANDED[@]:0:${give[2]}}"
    [ "${give[3]}" -gt 0 ] && printf '%s\n' "${RECENT[@]:0:${give[3]}}"
    [ "${give[4]}" -gt 0 ] && printf '%s\n' "${INFLOW[@]:0:${give[4]}}"
    [ "${give[5]}" -gt 0 ] && printf '%s\n' "${CI[@]:0:${give[5]}}"
    printf '%s\n' "${STANDING[@]}"
    return 0
}

# render <rows> <cols> — the frame, cut to what the pane can actually show.
#
# The share above already fits the elastic sections to the pane, so this is the backstop for
# the case it cannot solve: a pane too short even for one row of each section plus the
# standing figures. Seven lines were once written into five and the terminal scrolled — the
# top two vanished silently on every repaint, which is how the disk line and the ready total
# came to be invisible for weeks without anyone noticing they had been specified. Lines are
# emitted in priority order, the tail is dropped, and the DROP COUNT IS MARKED ON THE HEADER:
# a dashboard that hides content without saying so is the failure it exists to prevent.
# 0 rows means no limit, which is what `once` uses.
render() {
    local rows="${1:-0}" cols="${2:-0}" n drop
    local -a all
    mapfile -t all < <(frame "$rows" "$cols")
    n=${#all[@]}
    if [ "$rows" -gt 0 ] && [ "$n" -gt "$rows" ]; then
        drop=$(( n - rows ))
        all[0]="${all[0]}  ${C_DIM}▾${drop}${C_RST}"
        all=("${all[@]:0:$rows}")
    fi
    printf '%s\n' "${all[@]}"
}

# --- flicker-free painting -------------------------------------------------------
# `\e[2J` (clear screen) then redraw is what makes a pane flicker: for one frame the
# terminal shows nothing, and the eye catches it every tick. Four fixes together:
#
#   1. NEVER clear. Home the cursor, then erase each line as it is rewritten (\e[K)
#      and erase whatever is left below (\e[J). The old frame is overwritten in place,
#      so no blank frame ever exists.
#   2. NO TRAILING NEWLINE on the last line. A newline written in the bottom row scrolls
#      the pane by one, which is the other half of why the top lines were disappearing.
#   3. SKIP UNCHANGED FRAMES. Most ticks change nothing; comparing to the previous
#      frame turns those into zero writes. This is why the header shows the COLLECTION
#      time rather than the wall clock — a ticking second hand would defeat it.
#   4. SYNCHRONIZED OUTPUT (DECSET 2026). tmux buffers the whole update and presents it
#      atomically, so a repaint cannot be shown half-finished. Harmless where absent.
#   5. AUTOWRAP OFF (DECAWM). A line wider than the pane must be cut by the terminal, not
#      folded onto a second row: a folded line makes one printed line occupy two rows, and
#      the height arithmetic in `render` is then wrong by however many lines are long —
#      which is the scrolling bug arriving by a second route. Restored on exit.
#
# The cursor is hidden too — a block cursor parked in a dashboard is its own distraction.
# term_rows -> the pane's real height.
#
# `stty size` first, because it asks the TTY by ioctl and needs no terminfo: `tput lines`
# prints nothing and fails when $TERM is unset, which happens to anything launched
# detached — and its failure would fall through to "no limit", which is fail-OPEN on
# exactly the defect this function exists to close. When the height cannot be established
# at all, assume a small pane: under-rendering shows a ▾ count on the header and is visible
# and recoverable, while over-rendering scrolls the top lines away in silence. Five is
# deliberately pessimistic — the cockpit gives this pane a full column, so a fallback sized
# for one would be a guess that fails in the unsafe direction.
term_rows() {
    local r
    r="$(stty size 2>/dev/null | awk '{print $1}')"
    [ -n "${r:-}" ] || r="$(tput lines 2>/dev/null)"
    [ -n "${r:-}" ] || r="${LINES:-}"
    [ -n "${r:-}" ] && [ "$r" -gt 0 ] 2>/dev/null || r=5
    printf '%s' "$r"
}

# The same question in the other dimension, and it is asked for the same reason: the column
# is a percentage of a window whose width is the operator's, so nothing here may assume one.
# The fallback is 80 — the width every terminal has had since before any of this.
term_cols() {
    local c
    c="$(stty size 2>/dev/null | awk '{print $2}')"
    [ -n "${c:-}" ] || c="$(tput cols 2>/dev/null)"
    [ -n "${c:-}" ] || c="${COLUMNS:-}"
    [ -n "${c:-}" ] && [ "$c" -gt 0 ] 2>/dev/null || c=80
    printf '%s' "$c"
}

LAST_FRAME=""
paint() {
    local rows cols buf i n
    # Re-read every tick rather than trapping SIGWINCH: it is a couple of forks every two
    # seconds, and a resize the frame does not notice re-creates the scrolling bug silently.
    rows="$(term_rows)"; cols="$(term_cols)"
    buf="$(render "$rows" "$cols")"
    [ "$buf" = "$LAST_FRAME" ] && return
    LAST_FRAME="$buf"
    local -a lines
    mapfile -t lines <<< "$buf"
    n=${#lines[@]}
    {
        printf '\e[?2026h\e[H'
        for (( i = 0; i < n; i++ )); do
            printf '%s\e[K' "${lines[i]}"
            [ $(( i + 1 )) -lt "$n" ] && printf '\n'
        done
        printf '\e[J\e[?2026l'
    } 2>/dev/null
}
cleanup() { printf '\e[?25h\e[?7h\e[?2026l\n' 2>/dev/null; exit 0; }
trap cleanup INT TERM HUP EXIT

case "${1:-loop}" in
# An explicit row count makes the share observable from outside the pane. Without it a
# suite would have to fake a TTY of a given height to see how the rows were divided, and
# an allocator nobody has watched divide anything is a hypothesis.
once) render "${2:-0}" "${3:-0}" ;;
loop)
    printf '\e[?25l\e[?7l' 2>/dev/null
    _conf_file="${SPIRA_CONF_FILE:-}"
    _conf_mtime_0="$(stat --format='%Y' "$_conf_file" 2>/dev/null || echo 0)"
    while :; do
        paint
        sleep 2
        if [ -n "$_conf_file" ]; then
            _conf_mtime_now="$(stat --format='%Y' "$_conf_file" 2>/dev/null || echo 0)"
            if [ "$_conf_mtime_now" != "$_conf_mtime_0" ]; then
                printf 'health.sh: config changed — exiting for restart\n' >&2
                exit 0
            fi
        fi
    done
    ;;
*) echo "usage: health.sh [once [rows [cols]]|loop]" >&2; exit 1 ;;
esac
