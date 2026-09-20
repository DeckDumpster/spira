#!/usr/bin/env bash
#
# yield.sh — what the landing gate is WORTH, recorded beside what it costs.
#
#   yield.sh record <repo> <branch> <status> <reason> [tree] [suite]
#   yield.sh pass   <repo> <branch> [tree]
#   yield.sh classify <branch|record> <DEFECT|GATE_FAULT|UNKNOWN> [why...]
#   yield.sh report [--window <seconds>]      KEY=VALUE, for a pane or a sweep
#   yield.sh show   [--window <seconds>]      the same numbers, for a person
#   yield.sh list   [--window <seconds>]      one line per red, newest first
#
# WHY THIS EXISTS. A gate ran for weeks at seventeen minutes a branch, and on the morning it
# mattered it found zero real defects and produced two failures that were its own suites
# reading the state of the box. Every one of those facts was derivable from data the harness
# was already writing, and nobody knew any of them until the queue stopped and a human went
# looking. The gate was not deleted because it was measured; it was deleted because it caused
# an outage. A gate may sit between work and its landings only while it is catching real
# defects (law-gate-earns-its-place), and that sentence is worth nothing without a number.
#
# THE METRIC. Every gate red gets exactly one of three verdicts, and the third is not a
# rounding error:
#
#   DEFECT      the branch was genuinely wrong and the gate was right to refuse it
#   GATE_FAULT  the suite read the box, the base was already broken, a fixture collided, a
#               deadline fired — the branch did not cause it
#   UNKNOWN     nobody ever said
#
# UNKNOWN IS RENDERED, NEVER FOLDED INTO EITHER (law-absence-needs-a-positive-control). A
# measurement that quietly resolved its own unknowns towards "the gate was right" would be a
# gate grading its own homework, and a rising UNKNOWN count is the signal that this
# measurement has itself stopped working — which is a thing that must be visible rather than
# inferred from a suspiciously tidy ratio.
#
# WHERE THE VERDICT COMES FROM, AND WHY IT IS A BYPRODUCT. The only actor holding enough
# context to classify a red is whoever had to deal with it, at the time they dealt with it —
# a classification chore invented for a human to do later is a chore that stops being done in
# a fortnight. So two of the three sources cost nobody anything:
#
#   the gate's own taxonomy      BASE_FAIL and NO_VERDICT already MEAN "the branch is not at
#                                fault" (spira_gate_blames_branch). Those are GATE_FAULT on
#                                arrival, and they are the majority of a bad gate's reds.
#   what happened next           a red, then a PASS. If the tree is IDENTICAL, the verdict
#                                flipped with nothing changed, which is a gate fault by
#                                definition. If the tree CHANGED, somebody fixed something
#                                and the gate was right to refuse the old one.
#   somebody saying so           `classify`, which overrides either. This is what the aeon
#                                whose branch was refused runs when it knows better, and it
#                                is one command at the moment the knowledge exists.
#
# THE INFERRED HALF IS MARKED AS INFERRED. `by=` records which of the three said it, and the
# report carries the inferred DEFECT count separately, because "the branch changed and then
# passed" is strong evidence and not proof — a rebase changes the tree too. A reader who
# cannot tell a stated verdict from a deduced one will eventually believe a deduction that
# was wrong, and this is cheaper than that.
#
# THE RECORD IS ONE FILE PER RED, NAMED AFTER WHAT IT IS ABOUT — repository, branch, tree,
# reason. Two gates racing on one branch produce one row, not two, because they are one fact;
# the same branch refused for a second reason produces a second row, because it is two. The
# names are legible on purpose: the first thing anybody does with a directory like this is
# `ls` it, and a directory of hashes answers nothing.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# UNDER THE RUNTIME DIRECTORY AND SO NOT A CONFIG KEY, by the same rule as the verdict cache
# and the gate log: the harness put it there, and a colleague who moves SPIRA_RUN moves this
# with it. The environment may still point it somewhere else, which is the seam the suite
# drives.
YDIR="${SPIRA_YIELD:-$SPIRA_RUN/gate-yield}"
GATE_LOG="${SPIRA_GATE_LOG:-$SPIRA_RUN/gate.log}"
# THE WINDOW IS A DURATION AND IT IS NAMED WHEREVER IT IS RENDERED. "Recently" is the word
# that let the watchtower present a decommissioned mechanism's worst case as a live signal
# for a day; a reader who can see the bound can tell a quiet day from a broken probe.
WINDOW="${SPIRA_YIELD_WINDOW:-86400}"
# How long a red is kept. Long enough that a weekly look back is possible, bounded because
# nothing else ever deletes here and one file per red forever is the shape that fills a disk
# quietly.
KEEP_DAYS="${SPIRA_YIELD_KEEP_DAYS:-30}"

# fold <text> -> a filename component. Every character outside a safe set becomes `_`, so a
# branch with a slash in it — which every branch here has — does not become a directory tree.
fold() { printf '%s' "$1" | tr -c 'A-Za-z0-9._-' '_'; }

# rec_key <repo> <branch> <tree> <reason> -> the record's filename.
# The tree is truncated: twelve hex is far past collision for the handful of trees one branch
# ever has, and a full object id makes every name too long to read at a glance.
rec_key() {
    printf '%s.%s.%s.%s' "$(fold "$1")" "$(fold "$2")" "$(fold "${3:0:12}")" "$(fold "$4")"
}

# get <file> <key> -> the value, or nothing. Pure bash: a red is rare but `report` reads every
# record on every pane repaint, and three subprocesses per field is how a cheap probe becomes
# the thing that is slow during an outage.
get() {
    local k v
    while IFS='=' read -r k v; do
        [ "$k" = "$2" ] && { printf '%s' "$v"; return 0; }
    done < "$1" 2>/dev/null
    return 1
}

# THE BEAD IS TAKEN FROM THE CALLER WHEN THE CALLER KNOWS IT, and derived from the branch
# only when it does not. The landing pass is iterating beads and passes SPIRA_GATE_BEAD; an
# aeon running its own gate through the runner has only the branch, whose last component is
# the bead id under the derived-name convention (law-branch-affinity-is-recorded). A branch
# that does not look like one named after a bead records `-` rather than a guess.
bead_of() {
    local br="$1" tail
    if [ -n "${SPIRA_GATE_BEAD:-}" ]; then printf '%s' "$SPIRA_GATE_BEAD"; return; fi
    tail="${br##*/}"
    case "$tail" in
        [a-z][a-z]*-[a-z0-9]*) printf '%s' "$tail" ;;
        *) printf '-' ;;
    esac
}

prune() {
    [ -d "$YDIR" ] || return 0
    find "$YDIR" -maxdepth 1 -type f -mtime "+$KEEP_DAYS" -delete 2>/dev/null || true
}

# ---------------------------------------------------------------------------------------
# record — a gate refused a branch.
# ---------------------------------------------------------------------------------------
cmd_record() {
    local repo="$1" br="$2" status="$3" reason="${4:--}" tree="${5:--}" suite="${6:--}"
    local outcome verdict by why f now
    outcome="$(spira_gate_outcome "$status")"
    [ "$outcome" = PASS ] && return 0
    # THE GATE'S OWN TAXONOMY IS EVIDENCE, NOT A GUESS. BASE_FAIL says the same suites fail on
    # the base and NO_VERDICT says nothing was judged at all; both are already the harness's
    # settled answer to "is the branch at fault", and the landing pass acts on that answer by
    # refusing to charge an attempt. Recording them as anything but GATE_FAULT here would be
    # two parts of one program disagreeing about a question one of them has already decided.
    if spira_gate_blames_branch "$status"; then
        verdict=UNKNOWN; by=""; why=""
    else
        verdict=GATE_FAULT; by="auto:$outcome"
        why="the gate itself says the branch is not at fault — $outcome/$reason"
    fi
    mkdir -p "$YDIR" 2>/dev/null || return 0
    f="$YDIR/$(rec_key "$repo" "$br" "$tree" "$reason")"
    now="$(date +%s)"
    if [ -r "$f" ]; then
        # SEEN AGAIN IS NOT SEEN TWICE. Every bead pays this gate at least twice — the aeon
        # before it closes and the landing pass before it merges — so counting arrivals would
        # double every red and treble some, and the ratio this whole file exists to report
        # would be a ratio of how many times each branch was gated.
        local seen; seen="$(get "$f" seen)" || seen=1
        case "$seen" in ''|*[!0-9]*) seen=1 ;; esac
        sed -i "s/^seen=.*/seen=$((seen+1))/; s/^last=.*/last=$now/" "$f" 2>/dev/null
        return 0
    fi
    { printf 'at=%s\n'      "$now"
      printf 'last=%s\n'    "$now"
      printf 'seen=1\n'
      printf 'repo=%s\n'    "$repo"
      printf 'branch=%s\n'  "$br"
      printf 'bead=%s\n'    "$(bead_of "$br")"
      printf 'tree=%s\n'    "$tree"
      printf 'outcome=%s\n' "$outcome"
      printf 'reason=%s\n'  "$reason"
      printf 'suite=%s\n'   "$suite"
      printf 'verdict=%s\n' "$verdict"
      printf 'by=%s\n'      "$by"
      printf 'why=%s\n'     "$why"
    } > "$f.part" 2>/dev/null && mv -f "$f.part" "$f" 2>/dev/null
    prune
}

# ---------------------------------------------------------------------------------------
# pass — a gate let the same branch through. This is the byproduct classification, and it is
# the reason nobody has to remember to classify anything.
# ---------------------------------------------------------------------------------------
cmd_pass() {
    local repo="$1" br="$2" tree="${3:--}" f v rt
    [ -d "$YDIR" ] || return 0
    for f in "$YDIR"/*; do
        [ -r "$f" ] || continue
        [ "$(get "$f" repo)"   = "$repo" ] || continue
        [ "$(get "$f" branch)" = "$br"   ] || continue
        v="$(get "$f" verdict)" || v=""
        # ONLY AN UNKNOWN IS FILLED IN. A verdict somebody stated, or one an earlier pass
        # already deduced, is not revisited: a later gate run knows strictly less about an old
        # red than whoever was looking at it, and a mechanism that overwrites a human's answer
        # with its own inference is one nobody will use twice.
        [ "$v" = UNKNOWN ] || continue
        rt="$(get "$f" tree)" || rt="-"
        if [ -n "$tree" ] && [ "$tree" != "-" ] && [ "$rt" = "$tree" ]; then
            set_verdict "$f" GATE_FAULT auto:same-tree-passed \
                "this identical tree passed a later gate with nothing about the branch changed"
        else
            set_verdict "$f" DEFECT auto:changed-then-passed \
                "the branch was refused, then changed, and the changed branch passed"
        fi
    done
}

set_verdict() {          # set_verdict <file> <verdict> <by> <why...>
    local f="$1" v="$2" by="$3"; shift 3
    # ONE LINE, ALWAYS. The record is `key=value` per line and a reader splits on the first
    # `=`, so a newline inside a value silently becomes a key nobody wrote — and the field
    # after it disappears.
    local why; why="$(printf '%s' "$*" | tr '\n' ' ')"
    # sed over the three fields rather than a rewrite, so a concurrent reader never sees a
    # record without a verdict — which `report` would count as neither, silently shrinking
    # the denominator.
    sed -i "s|^verdict=.*|verdict=$v|; s|^by=.*|by=$by|; s|^why=.*|why=$why|" "$f" 2>/dev/null
}

# ---------------------------------------------------------------------------------------
# classify — somebody says which it was. Overrides an inference, deliberately.
# ---------------------------------------------------------------------------------------
cmd_classify() {
    local which="$1" v="$2"; shift 2
    local why="${*:-stated by $(id -un 2>/dev/null || echo someone)}" f n=0
    case "$v" in
        DEFECT|GATE_FAULT|UNKNOWN) ;;
        *) echo "yield: a verdict is DEFECT, GATE_FAULT or UNKNOWN — not '$v'" >&2; return 1 ;;
    esac
    [ -d "$YDIR" ] || { echo "yield: no gate reds have been recorded ($YDIR does not exist)" >&2; return 1; }
    for f in "$YDIR"/*; do
        [ -r "$f" ] || continue
        # Addressed by branch OR by the record's own name, because both are things the caller
        # actually has: an aeon knows its branch and nothing else, while somebody reading
        # `list` has the name in front of them.
        if [ "$(get "$f" branch)" = "$which" ] || [ "$(basename "$f")" = "$which" ]; then
            set_verdict "$f" "$v" "${SPIRA_YIELD_BY:-stated}" "$why"
            printf 'yield: %s -> %s\n' "$(basename "$f")" "$v"
            n=$((n+1))
        fi
    done
    [ "$n" -gt 0 ] || { echo "yield: nothing recorded for '$which'" >&2; return 1; }
}

# ---------------------------------------------------------------------------------------
# THE COST, AS A DISTRIBUTION AGAINST CONCURRENCY.
#
# A landing gate never runs solo. Every aeon runs one before it closes and the landing pass
# runs one per branch it is about to merge, so concurrency is the gate's OPERATING CONDITION
# and not an edge case (law-fixtures-carry-real-cadence). Quoting a solo figure is how a gate
# gets adopted at 101s and turns out to cost 329s in the condition it actually runs in, and
# it is the 3.3x gap rather than either number that decides whether it is affordable.
#
# CONCURRENCY IS DERIVED FROM THE LOG, NOT RECORDED. Each meter row carries the finish time,
# the seconds spent waiting for the tree and the seconds spent running, so a row's interval is
# [ts - waited - ran, ts] and two runs of one repository were concurrent exactly when their
# intervals overlap. Nothing new has to be written, which means this reads correctly over
# every row already on disk rather than only over rows written after it shipped.
#
# CACHED PASSES ARE EXCLUDED. A reused verdict neither queues for the tree nor runs a suite;
# it is a row about work that was skipped, and leaving it in drags the median towards zero and
# hides the cost this is here to show. It is excluded by its reason, which is the gate's own
# word for it, rather than by being fast.
# ---------------------------------------------------------------------------------------
cost_report() {
    local win="$1"
    # A FIELD THAT CANNOT BE READ RENDERS `?`, NEVER 0. "The gate costs 0s" is the reassuring
    # reading and it is what both a missing log and a missing interpreter would produce.
    if ! command -v python3 >/dev/null 2>&1 || [ ! -r "$GATE_LOG" ]; then
        printf 'YIELD_SOLO_N=?\nYIELD_SOLO_MED=?\nYIELD_SOLO_MAX=?\n'
        printf 'YIELD_CONC_N=?\nYIELD_CONC_MED=?\nYIELD_CONC_MAX=?\n'
        printf 'YIELD_LOG_REDS=?\n'
        return 0
    fi
    python3 - "$GATE_LOG" "$win" <<'PY'
import sys, re, calendar, time, signal
# A reader that pipes this into `head` closes the pipe, and python answers a closed
# stdout with a traceback on stderr — which lands in the middle of a gate's output and
# reads as the gate having broken. Default SIGPIPE makes it exit quietly, as every
# other program in the pipeline already does.
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
path, win = sys.argv[1], int(sys.argv[2])
now = time.time()
rows = []
pat = re.compile(r'^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z) +(\S+) +(\S+) +waited=(\d+)s +ran=(\d+)s +rc=(\S+)(?: +(.*))?$')
try:
    with open(path) as fh:
        for line in fh:
            m = pat.match(line.rstrip('\n'))
            if not m:
                continue                     # a truncated write or an older format: not now
            ts = calendar.timegm(time.strptime(m.group(1), '%Y-%m-%dT%H:%M:%SZ'))
            repo, waited, ran, note = m.group(2), int(m.group(4)), int(m.group(5)), (m.group(7) or '')
            if note.split(' ')[0] == 'cached':
                continue                     # a skipped gate is not a gate's cost
            rows.append((repo, ts - waited - ran, ts, waited + ran, m.group(6) != '0'))
except OSError:
    print('YIELD_SOLO_N=?\nYIELD_SOLO_MED=?\nYIELD_SOLO_MAX=?')
    print('YIELD_CONC_N=?\nYIELD_CONC_MED=?\nYIELD_CONC_MAX=?')
    print('YIELD_LOG_REDS=?')
    raise SystemExit
solo, conc, log_reds = [], [], 0
for i, (repo, s, e, cost, red) in enumerate(rows):
    if e < now - win:
        continue
    if red:
        log_reds += 1
    overlapped = any(r == repo and j != i and s2 < e and s < e2
                     for j, (r, s2, e2, _c, _r) in enumerate(rows))
    (conc if overlapped else solo).append(cost)
def stat(v):
    if not v:
        return ('0', '?', '?')               # no rows is not a measurement of zero seconds
    v = sorted(v)
    return (str(len(v)), str(v[len(v)//2]), str(v[-1]))
for name, v in (('SOLO', solo), ('CONC', conc)):
    n, med, mx = stat(v)
    print('YIELD_%s_N=%s' % (name, n))
    print('YIELD_%s_MED=%s' % (name, med))
    print('YIELD_%s_MAX=%s' % (name, mx))
# HOW MANY REDS THE METER ITSELF SAW, which is the positive control for everything above. The
# counts come from a record the gate has to remember to write; this comes from the row the
# gate writes on EVERY exit whether it is being measured or not. When one says "there were
# reds" and the other says "there were none", the recorder has stopped and the reassuring
# column is the wrong one.
print('YIELD_LOG_REDS=%d' % log_reds)
PY
}

# ---------------------------------------------------------------------------------------
# report — KEY=VALUE, for the Ops sweep and the pane. Nothing here is coloured or aligned:
# two readers render it and they disagree about width.
# ---------------------------------------------------------------------------------------
cmd_report() {
    local win="$WINDOW" f v at cutoff
    [ "${1:-}" = "--window" ] && { win="$2"; shift 2; }
    cutoff=$(( $(date +%s) - win ))
    printf 'YIELD_WINDOW=%s\n' "$win"
    # THE COST IS COMPUTED FIRST because it carries the positive control below. It is PRINTED
    # last, after the counts, so the two blocks read in the order a person wants them.
    local cost log_reds
    cost="$(cost_report "$win")"
    log_reds="$(printf '%s\n' "$cost" | sed -n 's/^YIELD_LOG_REDS=//p' | head -1)"
    # THE DIRECTORY'S ABSENCE IS UNREADABLE, NOT EMPTY. A gate that has never recorded a red
    # and a recorder that was never wired in look identical from here, and one of them is the
    # measurement being broken. Only the second is reported as `?` — the directory is created
    # by the first `record`, so once anything has ever been recorded, "no reds in the window"
    # is a real and reassuring reading.
    if [ ! -d "$YDIR" ]; then
        printf 'YIELD_REDS=?\nYIELD_DEFECT=?\nYIELD_FAULT=?\nYIELD_UNKNOWN=?\n'
        printf 'YIELD_DEFECT_INFERRED=?\nYIELD_TOP_FAULT=?\n'
        # NO RECORD AT ALL IS TWO DIFFERENT FACTS, and the meter tells them apart. On a box
        # where the gate has simply never refused anything, the recorder is merely `absent`
        # and there is nothing wrong. Where the meter has logged reds and no record exists,
        # the recorder is not running — which is the failure, and saying `?` for it would
        # send the reader to look at the meter, the one half that is working.
        case "$log_reds" in
            ''|'?') printf 'YIELD_RECORDER=?\n' ;;
            *) if [ "$log_reds" -gt 0 ] 2>/dev/null
               then printf 'YIELD_RECORDER=silent\n'
               else printf 'YIELD_RECORDER=absent\n'; fi ;;
        esac
        printf '%s\n' "$cost"
        return 0
    fi
    local n=0 d=0 gf=0 u=0 di=0
    local -A faults=()
    for f in "$YDIR"/*; do
        [ -r "$f" ] || continue
        at="$(get "$f" at)" || continue
        case "$at" in ''|*[!0-9]*) continue ;; esac
        [ "$at" -ge "$cutoff" ] || continue
        n=$((n+1))
        v="$(get "$f" verdict)" || v=UNKNOWN
        case "$v" in
            DEFECT)
                d=$((d+1))
                case "$(get "$f" by)" in auto:*) di=$((di+1)) ;; esac ;;
            GATE_FAULT)
                gf=$((gf+1))
                # WHICH SUITE, so a single suite responsible for a run of gate faults is
                # nameable and removable without touching the rest — which is the action this
                # number exists to make possible. A red with no suite is attributed to the
                # gate's own reason instead, because "which check keeps refusing good work"
                # has an answer either way.
                local k; k="$(get "$f" suite)" || k=""
                if [ -z "$k" ] || [ "$k" = "-" ]; then k="$(get "$f" reason)" || k=""; fi
                [ -n "$k" ] && faults["$k"]=$(( ${faults["$k"]:-0} + 1 )) ;;
            *)  u=$((u+1)) ;;
        esac
    done
    local top="-" topn=0 k
    for k in "${!faults[@]}"; do
        [ "${faults[$k]}" -gt "$topn" ] && { top="$k"; topn="${faults[$k]}"; }
    done
    [ "$topn" -gt 0 ] && top="$top x$topn"
    # =====================================================================================
    # THE POSITIVE CONTROL, AND THE ONLY THING THAT MAKES A ZERO ABOVE BELIEVABLE.
    #
    # The way this measurement fails is not that it reports a wrong number — it is that it
    # silently stops being called, and then reports a tidy `0 gate faults` forever. From the
    # outside that is indistinguishable from a gate with a perfect record, and it is the
    # reassuring one of the two readings, so nobody looks. Everything above comes from a
    # record the gate has to remember to write.
    #
    # The gate METER does not have to remember: it writes a row on every exit, including
    # every red, whether or not anything is measuring yield. So a window in which the meter
    # saw reds and the record holds none is proof the recorder is not running, and the counts
    # are then withheld as `?` rather than published as a clean sheet.
    #
    # THE COMPARISON IS ONLY AGAINST ZERO, deliberately. The two never agree on magnitude and
    # should not: a tree gated twice writes two meter rows and one record, because two
    # arrivals of one fact are one fact. Dedupe can take a count to one; it cannot take it to
    # none.
    # =====================================================================================
    local recorder=ok
    case "$log_reds" in
        ''|'?') recorder="?" ;;
        *) if [ "$log_reds" -gt 0 ] 2>/dev/null && [ "$n" -eq 0 ]; then recorder=silent; fi ;;
    esac
    if [ "$recorder" = silent ]; then
        printf 'YIELD_REDS=?\nYIELD_DEFECT=?\nYIELD_FAULT=?\nYIELD_UNKNOWN=?\n'
        printf 'YIELD_DEFECT_INFERRED=?\nYIELD_TOP_FAULT=?\n'
    else
        printf 'YIELD_REDS=%s\nYIELD_DEFECT=%s\nYIELD_FAULT=%s\nYIELD_UNKNOWN=%s\n' "$n" "$d" "$gf" "$u"
        printf 'YIELD_DEFECT_INFERRED=%s\nYIELD_TOP_FAULT=%s\n' "$di" "$top"
    fi
    printf 'YIELD_RECORDER=%s\n' "$recorder"
    printf '%s\n' "$cost"
}

cmd_list() {
    local win="$WINDOW" f at cutoff
    [ "${1:-}" = "--window" ] && { win="$2"; shift 2; }
    cutoff=$(( $(date +%s) - win ))
    [ -d "$YDIR" ] || { echo "yield: $YDIR does not exist — no gate red has ever been recorded" >&2; return 1; }
    for f in "$YDIR"/*; do
        [ -r "$f" ] || continue
        at="$(get "$f" at)" || continue
        case "$at" in ''|*[!0-9]*) continue ;; esac
        [ "$at" -ge "$cutoff" ] || continue
        printf '%s %-10s %-24s %-10s %-22s %s\n' \
            "$(date -u -d "@$at" +%Y-%m-%dT%H:%MZ 2>/dev/null || echo "$at")" \
            "$(get "$f" verdict)" "$(get "$f" branch)" "$(get "$f" outcome)" \
            "$(get "$f" reason)" "$(get "$f" why)"
    done | sort -r
}

cmd_show() {
    local win="$WINDOW"
    [ "${1:-}" = "--window" ] && { win="$2"; shift 2; }
    local out; out="$(cmd_report --window "$win")"
    eval "$(printf '%s\n' "$out" | sed -n 's/^\(YIELD_[A-Z_]*\)=\(.*\)$/\1="\2"/p')"
    # A UNIT ON AN UNREADABLE FIELD INVITES READING IT AS A MEASUREMENT. `?s` looks like a
    # duration somebody forgot to fill in; `?` looks like what it is.
    secs() { [ "${1:-?}" = "?" ] && printf '?' || printf '%ss' "$1"; }
    # WHEN THE COUNT CANNOT BE TRUSTED, THE LINE SAYS WHY. A `?` with no explanation sends the
    # reader to the code; the one sentence that names which of the two sources disagreed sends
    # them to the recorder.
    local recorder_note=""
    case "${YIELD_RECORDER:-?}" in
        silent) recorder_note="   <- the gate meter saw ${YIELD_LOG_REDS:-?} red(s) and none was recorded: THE RECORDER IS NOT RUNNING" ;;
        absent) recorder_note="   <- nothing has been recorded here yet, and the gate meter has logged no reds either" ;;
        '?')    recorder_note="   <- the gate meter could not be read, so this count has no positive control" ;;
    esac
    local label
    if [ "$win" -ge 86400 ] 2>/dev/null; then label="last $(( win / 86400 ))d"
    elif [ "$win" -ge 3600 ] 2>/dev/null; then label="last $(( win / 3600 ))h"
    else label="last $(( win / 60 ))m"; fi
    cat <<EOF
gate yield, $label — is the gate catching defects, or making them up?

  reds                 ${YIELD_REDS}${recorder_note}
    defect             ${YIELD_DEFECT}      (${YIELD_DEFECT_INFERRED} of them inferred from a later pass, not stated)
    gate fault         ${YIELD_FAULT}      worst: ${YIELD_TOP_FAULT}
    unknown            ${YIELD_UNKNOWN}      never classified — not counted as either

gate cost, $label — solo is not the condition it runs in

  solo                 $(secs "${YIELD_SOLO_MED}") median, $(secs "${YIELD_SOLO_MAX}") worst   (n=${YIELD_SOLO_N})
  contended            $(secs "${YIELD_CONC_MED}") median, $(secs "${YIELD_CONC_MAX}") worst   (n=${YIELD_CONC_N})

A field reading \`?\` is one this pass COULD NOT READ. Never treat it as a zero.
EOF
}

case "${1:-show}" in
    record)   shift; cmd_record "$@" ;;
    pass)     shift; cmd_pass "$@" ;;
    classify) shift; [ $# -ge 2 ] || { echo "usage: yield.sh classify <branch> <DEFECT|GATE_FAULT|UNKNOWN> [why]" >&2; exit 1; }
              cmd_classify "$@" ;;
    report)   shift; cmd_report "$@" ;;
    list)     shift; cmd_list "$@" ;;
    show|--show) shift; cmd_show "$@" ;;
    *) echo "usage: yield.sh record|pass|classify|report|list|show" >&2; exit 1 ;;
esac
