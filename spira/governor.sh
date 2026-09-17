#!/usr/bin/env bash
#
# governor.sh — how much of this machine Spira may use, and what it got for it.
#
#   governor.sh            write the budget, print one line
#   governor.sh --report   the human view: headroom, throughput, what is throttling
#
# WHY A SEPARATE ENTITY (the operator's call)
# -------------------------------------------
# "another entity keeping track of throughput to make sure we're within the resource
# constraints of this machine."
#
# The box this runs on may not be a build farm. It may also be running production services,
# two GitHub Actions runners, the whole of Gas Town, and the operator's own session. An aeon is an
# Opus session plus a cargo build plus a test suite, and nothing in the harness previously
# asked whether the machine could afford one — FAYTH_MAX_CONCURRENT is a COUNT, which is a
# proxy for load and not a measure of it.
#
# The budget is a deterministic function of what /proc says — averaged over the interval
# since the last pass, and folded into a moving average across passes. The first version
# decided from one two-second sample per pass, and its own history showed budgets of 0, 1
# and 2 within minutes at a constant aeon count: a two-second window lands on or between a
# gate's suites at random. The arithmetic lives in governor-budget.py so a fixture can drive
# it; this file reads /proc and remembers the last counters in budget.env. It never GROWS a
# fayth's own cap; it only withholds, so a fayth's concurrency stays the ceiling and this
# is the floor.
#
# THE ANSWER IS HEADROOM — how many MORE aeons may start. The old formula counted the aeons
# that would fit in the idle CPU, which is additional aeons since the running ones are
# already in the load, and lib.sh then read it as a total and subtracted the running count
# again: the more aeons ran, the more it withheld. SP_HEADROOM is what fayth_free clamps by;
# SP_BUDGET is running + headroom, a total, for readers that want one.
#
# IT MUST FAIL CLOSED-ISH, NOT OPEN. An unreadable probe yields the fayth's configured cap
# rather than infinity, and says so — but a probe that reads FINE and reports pressure
# must actually throttle. A governor that cannot be seen throttling has never been tested.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

BUDGET="$SPIRA_RUN/budget.env"
HIST="$SPIRA_RUN/governor.tsv"
# MEASURE FIRST, ENFORCE LATER (the operator, verbatim: "put the governor in 'measure' mode
# rather than 'enforce' mode until we know what load actually looks like with Spira and no
# Gastown"). Two metrics disagreed violently on the same instant — load average said 0
# aeons, measured CPU idle said 2 at 65% idle — and a throttle built on a number nobody
# trusts yet is a throttle that will be wrong in a direction nobody notices. In `measure`
# the budget is computed, recorded and logged, and clamps nothing. Flip to `enforce` when
# the history says what the ceiling should be, which is knowable only once Gas Town is
# gone and Spira is the load.
#
# MEASUREMENT OUTCOME (sp-fhq6, 2026-09-09). After >24h of governor.tsv history since the
# rewrite (1373 rows, 2026-09-07 to 2026-09-09), the smoothed idle_avg has a near-zero
# correlation with landings-per-hour (r=0.099) — it primarily reflects gate/build load
# rather than productive throughput. idle_avg averages 43.6%; idle changes only ~4pp between
# running=0 and running=3 (aeons spend 80%+ of their time waiting on the API, not burning
# CPU). The default is now ceil(50/cores) — half a core — giving 13% on a 4-core box
# (sp-vfx56, 2026-09-10). Governor remains in measure mode (sp-kvech verdict).
MODE="${SPIRA_GOVERNOR_MODE:-measure}"
# Load average is REPORTED and not decided on — see the note below the sample. The two
# knobs that once named a load ceiling were defined and never read; they are gone rather
# than left as a setting that does nothing.
MIN_FREE_MB="${SPIRA_MIN_FREE_MB:-1500}"
MIN_DISK_PCT="${SPIRA_MIN_DISK_PCT:-10}"

cores=$(host_cores)
load1=$(awk '{print $1}' /proc/loadavg 2>/dev/null || echo "?")

# TWO READINGS OF /proc/stat. The cumulative counters, held over to the next pass through
# budget.env, give the idle fraction for the whole interval between passes; the two-second
# sample is the fallback for the first pass and for counters that describe another era (a
# reboot, a gap past SPIRA_IDLE_MAX_INTERVAL). governor-budget.py picks, and says which.
stat_now="$(read -r _ a b c i _ < /proc/stat && echo "$((a+b+c+i)) $i")" 2>/dev/null || stat_now=""
stat_total="${stat_now%% *}"; stat_idle="${stat_now##* }"
cpu_sample=$(
  read -r _ a b c i _ < /proc/stat; t1=$((a+b+c+i)); i1=$i
  sleep 2
  read -r _ a b c i _ < /proc/stat; t2=$((a+b+c+i)); i2=$i
  [ "$((t2-t1))" -gt 0 ] && echo $(( (i2-i1)*100 / (t2-t1) )) || echo ""
) 2>/dev/null || cpu_sample=""
# Percent of total CPU that must stay free. An aeon is mostly waiting on the API, but its
# gate runs ten suites and its build runs cargo, so it is not free either.
IDLE_FLOOR="${SPIRA_IDLE_FLOOR:-25}"
# What one aeon is assumed to cost in idle points: one core's worth unless told otherwise.
IDLE_PER_AEON="${SPIRA_IDLE_PER_AEON:-}"
# How far one pass moves the average toward its reading. 0.25 with passes two minutes
# apart: a change in load is mostly believed after about ten minutes, and never on one glance.
IDLE_ALPHA="${SPIRA_IDLE_ALPHA:-0.25}"
IDLE_MAX_INTERVAL="${SPIRA_IDLE_MAX_INTERVAL:-900}"
memfree=$(awk '/MemAvailable/{printf "%d", $2/1024}' /proc/meminfo 2>/dev/null || echo "")
diskroot=$(df --output=pcent / 2>/dev/null | tail -1 | tr -dc '0-9' || echo "")
diskws=$(df --output=pcent "$SPIRA_WORKSPACES" 2>/dev/null | tail -1 | tr -dc '0-9' || echo "")

# The CI runners are the other heavy tenant and they serve a human waiting on a PR.
# Yield to them: an aeon can wait, a red build cannot.
# ACTIVE IS NOT BUSY, AND NEITHER IS "HAS A CHILD". The first version asked whether the
# runner's MainPID had any child — but an IDLE runner always has a run-helper.sh child, so
# this read busy forever, pinned the budget at 0, and would have starved the harness while
# every probe looked healthy. A governor that always says no is as broken as one that
# always says yes. Runner.Worker is the process that exists only while a job is running;
# argv from /proc decides, because pgrep may nominate.
ci_busy=0
for d in /proc/[0-9]*; do
    [ -r "$d/cmdline" ] || continue
    case "$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null)" in
        *Runner.Worker*) ci_busy=1; break ;;
    esac
done

# What the last pass left: its counters, its average, and when. Read in a subshell so a
# stale or hand-edited file cannot set anything else here.
prev="$( [ -r "$BUDGET" ] && . "$BUDGET" 2>/dev/null; printf '%s|%s|%s|%s' \
         "${SP_STAT_TOTAL:-}" "${SP_STAT_IDLE:-}" "${SP_CPU_IDLE_AVG:-}" "${SP_BUDGET_EPOCH:-}")"
IFS='|' read -r prev_total prev_idle prev_avg prev_epoch <<< "$prev"
[ "$prev_avg" = "?" ] && prev_avg=""
# NO AVERAGE YET, BUT A HISTORY: seed from the last eight readings on record rather than
# from whatever the next two seconds happen to hold. The first live pass of this governor
# seeded at 15% off one burst sample and then owed ten minutes of climbing for it.
if [ -z "$prev_avg" ] && [ -s "$HIST" ]; then
    prev_avg="$(tail -n 8 "$HIST" | awk -F'\t' '$5 ~ /^[0-9]+$/ {n++; t+=$5} END {if (n) printf "%.1f", t/n}')"
fi
now_epoch="$(date +%s)"

# The running count the way fayth_free counts it — pid files whose process is our runner —
# not systemd's unit list, which also holds units that have finished and not yet been reaped.
running=0
for _f in $(spira_fayths); do running=$(( running + $(aeon_count "$_f") )); done

# THE DECISION, from governor-budget.py. Every input is a number read above; every output
# is a KEY=value line, parsed here by name rather than by position.
verdict="$(STAT_TOTAL="$stat_total" STAT_IDLE="$stat_idle" \
           PREV_STAT_TOTAL="$prev_total" PREV_STAT_IDLE="$prev_idle" \
           PREV_AT="$prev_epoch" NOW="$now_epoch" MAX_INTERVAL="$IDLE_MAX_INTERVAL" \
           SAMPLE_IDLE="$cpu_sample" PREV_AVG="$prev_avg" ALPHA="$IDLE_ALPHA" \
           CORES="$cores" FLOOR="$IDLE_FLOOR" PER_AEON="$IDLE_PER_AEON" RUNNING="$running" \
           MEM_MB="$memfree" MIN_MEM_MB="$MIN_FREE_MB" \
           DISK_ROOT="$diskroot" DISK_WS="$diskws" MIN_DISK_PCT="$MIN_DISK_PCT" \
           CI_BUSY="$ci_busy" WS_PATH="$SPIRA_WORKSPACES" \
           python3 "$(dirname "$0")/governor-budget.py" 2>/dev/null)"
field() { printf '%s\n' "$verdict" | sed -n "s/^$1=//p"; }
cpu_idle="$(field IDLE)";      idle_source="$(field IDLE_SOURCE)"
idle_avg="$(field IDLE_AVG)";  headroom="$(field HEADROOM)"
budget="$(field BUDGET)";      reason="$(field REASON)"; per_aeon="$(field PER_AEON)"
# The arithmetic itself failing is the one probe this cannot name from inside: fail
# closed-ish, one aeon, and say so.
if [ -z "$budget" ]; then
    budget=1; headroom=1; cpu_idle="?"; idle_avg="?"; reason="governor-budget.py did not answer"
fi
[ -n "$memfree" ]  || memfree="?"
[ -n "$diskroot" ] || diskroot="?"
[ -n "$diskws" ]   || diskws="?"

# ---- throughput: what the spend bought -----------------------------------------------
# ACROSS EVERY REPOSITORY. This is the "what did the spend buy" side of the budget, and an
# aeon's commits land wherever its bead named — counting one repository would read a busy
# harness as an idle one and hand it capacity it was already using.
landed_24h=0; landed_1h=0
for _r in $(spira_repos); do
    _p="$(repo_root "$_r")" || continue
    [ -e "$_p/.git" ] || continue
    landed_24h=$(( landed_24h + $(git -C "$_p" log --since=24.hours --format='%an' 2>/dev/null | grep -c '^aeon-' || true) ))
    landed_1h=$((  landed_1h  + $(git -C "$_p" log --since=1.hour  --format='%an' 2>/dev/null | grep -c '^aeon-' || true) ))
done
aeon_min=$(systemctl --user list-units 'spira-aeon-*' --all --no-legend 2>/dev/null | wc -l)

cat > "$BUDGET" <<EOF
SP_BUDGET='$budget'
SP_HEADROOM='$headroom'
SP_GOVERNOR_MODE='$MODE'
SP_BUDGET_REASON='$reason'
SP_CORES='$cores'
SP_LOAD1='$load1'
SP_CPU_IDLE='$cpu_idle'
SP_CPU_IDLE_SOURCE='$idle_source'
SP_CPU_IDLE_AVG='$idle_avg'
SP_IDLE_PER_AEON='$per_aeon'
SP_AEON_RUNNING='$running'
SP_STAT_TOTAL='$stat_total'
SP_STAT_IDLE='$stat_idle'
SP_BUDGET_EPOCH='$now_epoch'
SP_MEM_AVAIL_MB='$memfree'
SP_DISK_ROOT_PCT='$diskroot'
SP_DISK_WS_PCT='$diskws'
SP_CI_BUSY='$ci_busy'
SP_LANDED_1H='$landed_1h'
SP_LANDED_24H='$landed_24h'
SP_AEON_UNITS='$aeon_min'
SP_BUDGET_AT='$(date -u +%Y-%m-%dT%H:%M:%SZ)'
EOF

# One row per pass. This is the evidence that decides the eventual ceiling; without it
# "what does load look like" is a memory of a few glances at a terminal.
# Rows written before the average existed have ten columns; the header is brought up to
# date in place so a reader parsing by name finds the new ones, and the old rows stay as
# they were — they are the record of what the two-second governor decided.
HDR='ts\tmode\tbudget\treason\tcpu_idle\tload1\tcores\tmem_mb\tci\tlanded_1h\tidle_avg\theadroom\trunning\tidle_source'
[ -s "$HIST" ] || printf "$HDR\n" > "$HIST"
head -1 "$HIST" | grep -q 'idle_avg' || sed -i "1s/.*/$HDR/" "$HIST"
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$MODE" "$budget" "$reason" "$cpu_idle" "$load1" \
    "$cores" "$memfree" "$ci_busy" "$landed_1h" "$idle_avg" "$headroom" "$running" "$idle_source" >> "$HIST"

if [ "${1:-}" = "--report" ]; then
    printf 'governor [%s]: %s more aeon(s) may start (%s running, %s in all) — %s\n' \
        "$MODE" "$headroom" "$running" "$budget" "$reason"
    printf '  cpu %s%% idle avg (%s%% this %s, floor %s%%, %s%% per aeon) · load %s of %s cores · %sMB free · / %s%% · %s %s%% · CI busy: %s\n' \
        "$idle_avg" "$cpu_idle" "$idle_source" "$IDLE_FLOOR" "$per_aeon" "$load1" "$cores" "$memfree" "$diskroot" "$SPIRA_WORKSPACES" "$diskws" "$ci_busy"
    printf '  landed by aeons: %s in the last hour, %s in 24h\n' "$landed_1h" "$landed_24h"
else
    printf 'governor: headroom=%s running=%s (%s) idle_avg=%s load=%s/%s mem=%sMB landed24h=%s\n' \
        "$headroom" "$running" "$reason" "$idle_avg" "$load1" "$cores" "$memfree" "$landed_24h"
fi
