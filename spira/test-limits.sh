#!/usr/bin/env bash
#
# test-limits.sh — the two rate-limit windows: the samples, the projection, and what is shown
# when there is nothing to project from.
#
#   ./test-limits.sh
#
# WHAT THIS SUITE IS GUARDING. These two numbers are the ones that end a working day, so the
# expensive failure is not a missing segment but a reassuring one: a window rendered at 0%
# because the client sent none, or a fill projected across a reset boundary, is good news
# nobody measured. Every case that asserts something is ABSENT therefore first proves the same
# code path renders it when it is present (law-absence-needs-a-positive-control), and the
# `-`/`?`/0 distinction is asserted directly rather than assumed.
#
# THE FIRST CASE IS THE REGRESSION, AND IT IS FIRST ON PURPOSE. The suite that passed while the
# meter rendered a median of 250 %/h fed samples five minutes apart — a cadence the hook never
# produces. Evenly spaced readings are the one input on which every estimator agrees, so a
# suite built from them cannot see the defect that lives in the real cadence. The fixture below
# is a verbatim capture (law-fixtures-carry-real-cadence).
#
# THE CLOCK IS INJECTED. A projection is a statement about a moment, so a suite that used the
# real clock would be asserting against whenever it happened to run — and the reset-boundary
# case needs samples minutes apart without waiting minutes. `SPIRA_NOW` is read from the
# environment and is deliberately not a config key, so nothing an operator writes in spira.conf
# can freeze the clock of a live installation.
#
# AND IT RUNS UNDER `env -i`. A suite that inherits the operator's spira.conf writes samples
# into their real runtime directory and asserts against their real account
# (law-gates-run-in-a-clean-environment).
#
# defect: sp-0uu
# tier: T1
# covers: spira/ctx-meter.sh
# scar: the rate-limit window projected from evenly-spaced samples, a cadence the hook never produces; the real uneven cadence produced a wrong fill projection.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
CTX="$HERE/ctx-meter.sh"

has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "no [$3] in [$2]" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "found [$3] in [$2]" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/home" "$T/run" "$T/projects/-a-project"

# The fixture's knobs, all pinned away from the shipped defaults: asserting against a default
# passes just as well with the value written back into the code, which is what the key exists
# to stop. NONE points the config reader at a path that does not exist, which is how it is told
# to read no file at all.
NONE="$T/no-such.conf"
CW=900; CH=1800; CL=2700
SAMPLES="$T/run/limits.samples"

# EPOCH is a fixed instant the whole suite reasons from, so every expectation is arithmetic on
# it rather than on whenever the suite ran.
EPOCH=1750000000

# meter <mode> <at-epoch> [arg] — the meter under a minimal environment and a frozen clock.
# stdin is the hook blob for `line` mode and is ignored by `env`.
meter() {
    local mode="$1" at="$2"; shift 2
    env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" \
        SPIRA_RUN="$T/run" SPIRA_TOKEN_PROJECTS="$T/projects" \
        SPIRA_CTX_WARN="$CW" SPIRA_CTX_HIGH="$CH" SPIRA_CTX_LIMIT="$CL" \
        SPIRA_NOW="$at" \
        bash "$CTX" "$mode" "$@" 2>/dev/null
}
val() { sed -n "s/^$1=//p"; }

# blob <five-pct> <five-reset> <seven-pct> <seven-reset> — a status-line payload carrying the
# windows named. A window is omitted entirely when its percentage is given as `-`, which is how
# the client reports one that has not appeared yet or has already reset.
#
# THE SHAPE IS THE ONE CAPTURED FROM A REAL CLIENT, not one invented here: rate_limits.
# {five_hour,seven_day}.{used_percentage,resets_at}, integers, alongside context_window.
blob() {
    python3 - "$@" <<'PY'
import json, sys
p5, r5, p7, r7 = sys.argv[1:5]
rl = {}
if p5 != "-": rl["five_hour"]  = {"used_percentage": int(p5), "resets_at": int(r5)}
if p7 != "-": rl["seven_day"] = {"used_percentage": int(p7), "resets_at": int(r7)}
out = {
    "session_id": "s-1",
    "transcript_path": sys.argv[5],
    "cwd": "/tmp",
    "context_window": {"total_input_tokens": 1200, "current_usage": {"input_tokens": 1200},
                       "used_percentage": 1, "context_window_size": 1000000},
}
if rl: out["rate_limits"] = rl
print(json.dumps(out))
PY
}

# One transcript so the ctx half of the line has something to say; this suite asserts nothing
# about it beyond that the limits segment is appended and not substituted for it.
TP="$T/projects/-a-project/s-1.jsonl"
printf '{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":200,"cache_creation_input_tokens":0,"cache_read_input_tokens":1000,"output_tokens":10}}}\n' > "$TP"

# feed <at> <p5> <r5> <p7> <r7> -> the rendered line
feed() { blob "$2" "$3" "$4" "$5" "$TP" | meter line "$1"; }

strip() { sed 's/\x1b\[[0-9;]*m//g'; }

# rate_of <line> -> the rendered %/h as a bare number, or "" when no rate was rendered
rate_of() { printf '%s' "$1" | sed -n 's/.*↑\([0-9.]*\)%\/h.*/\1/p'; }
# between <name> <lo> <hi> <value> — a numeric band, so an assertion about a MEASUREMENT is not
# written as string equality against one rendering of it.
between() {
    awk -v v="$4" -v lo="$2" -v hi="$3" 'BEGIN{exit !(v != "" && v+0 >= lo && v+0 <= hi)}' \
        && ok "$1 ($4)" || bad "$1" "got [$4], wanted $2..$3"
}

# ==========================================================================================
echo
echo "the quantised burst — the cadence the hook actually produces"
# ==========================================================================================
# CAPTURED VERBATIM from a live limits.samples: long flat runs, then a 1-point step landing two
# to five seconds after that run's last confirmation. 33% to 39% across 26.9 minutes is a true
# burn of 13.4 %/h.
#
# THAT STEP IS THE WHOLE DEFECT. `used_percentage` is rounded to whole percent, so the pair
# straddling a step is one point measured across three seconds — 1200 %/h — and any estimator
# that differentiates pairwise is seeded with that and never pays it off. A per-interval EMA
# returns 209.8 %/h on exactly this input; the span rate returns 13.4.
BURST="1788756787 33 1788765600 21 1788807600
1788756790 34 1788765600 21 1788807600
1788757265 34 1788765600 21 1788807600
1788757267 35 1788765600 21 1788807600
1788757468 35 1788765600 21 1788807600
1788757470 36 1788765600 21 1788807600
1788757832 36 1788765600 21 1788807600
1788757832 37 1788765600 21 1788807600
1788757985 37 1788765600 21 1788807600
1788757986 37 1788765600 22 1788807600
1788758050 37 1788765600 22 1788807600
1788758055 38 1788765600 22 1788807600
1788758278 38 1788765600 22 1788807600
1788758281 39 1788765600 22 1788807600
1788758398 39 1788765600 22 1788807600"
BURST_END=1788758398

# REPLAYED THROUGH THE RECORDER, not planted, so the run-collapsing and the pruning are on the
# path this case measures rather than assumed away.
rm -f "$SAMPLES"
while read -r ts p5 r5 p7 r7; do
    feed "$ts" "$p5" "$r5" "$p7" "$r7" >/dev/null
done <<<"$BURST"
is "the cadence was recorded as fed" "15" "$(wc -l < "$SAMPLES")"

# The window's own reset is two hours out and the fill is four and a half, so with the captured
# resets_at the limit does not bite and nothing is projected. That is correct and it is also
# why the rate itself is read from `env`, which publishes the projection regardless of which
# comes first.
L="$(feed "$BURST_END" 39 1788765600 22 1788807600 | strip)"
hasnt "a window that resets before it fills projects nothing" "$L" "full in"
E="$(meter env "$BURST_END")"
ETA="$(val SP_LIMIT_5H_ETA <<<"$E")"
# 61 points left at the true 13.4 %/h is 273 minutes. ±30% of the RATE is what the band below
# expresses, converted to the seconds the key publishes: a faster estimate is a shorter ETA.
between "the published projection implies the measured burn" 12600 23500 "$ETA"

# Now the same cadence with the reset pushed beyond the fill — one field changed, and the only
# one — so the clause renders and the rate is on screen where it can be asserted directly.
L="$(feed "$BURST_END" 39 $((BURST_END + 86400)) 22 1788807600 | strip)"
R="$(rate_of "$L")"
between "the rendered rate is within 30% of the true 13.4 %/h" 9.38 17.42 "$R"
# THE CEILING IS THE POINT. 60 %/h is far above anything this fixture can honestly produce and
# far below what any pairwise estimator does produce, so it is the assertion that the old
# estimator cannot pass.
between "and it is nowhere near a pairwise estimate"          0    59.99 "$R"

# SHOWN ABLE TO FAIL. The band above is only meaningful if the estimator it rejects would in
# fact be rejected, so the discarded one is run over the same fixture here and required to land
# outside it. A regression case that both estimators pass is not testing the regression.
# The estimator lives in a FILE rather than a heredoc, because a heredoc supplying the script
# and a herestring supplying its samples both claim stdin, and the redirection that wins feeds
# the fixture to the interpreter as though it were python.
cat > "$T/ema.py" <<'PY'
import math, sys
pts = [(int(l.split()[0]), float(l.split()[1])) for l in sys.stdin if l.strip()]
ema = None
for (t0, p0), (t1, p1) in zip(pts, pts[1:]):
    dt = (t1 - t0) / 60.0
    if dt <= 0: continue
    slope = (p1 - p0) / dt
    ema = slope if ema is None else ema + (1.0 - math.exp(-dt / 15.0)) * (slope - ema)
print("%.1f" % (ema * 60))
PY
EMA="$(python3 "$T/ema.py" <<<"$BURST")"
awk -v v="$EMA" 'BEGIN{exit !(v+0 >= 60)}' \
    && ok "the per-interval EMA fails the same band ($EMA %/h)" \
    || bad "the per-interval EMA fails the same band" "got [$EMA], expected >= 60"

# ==========================================================================================
echo
echo "the projection — a climbing window, and when it is full"
# ==========================================================================================
# Three samples five minutes apart, 60 -> 62 -> 64%. That is 0.4 points a minute, and from 64%
# there are 36 points left, so the window is full in 90 minutes. The reset is a day away, so it
# cannot be what the line reports.
rm -f "$SAMPLES"
feed "$EPOCH"            60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 300))"  62 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 64 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"

has "the 5-hour window is rendered with its percentage" "$L" "64%"
has "and the measured rate that justifies the warning"  "$L" "↑24.0%/h"
# ±5 minutes of 1h30m, asserted as the set of renderings that satisfies it rather than by
# re-deriving the arithmetic here — a test that recomputes the formula passes when the formula
# is wrong in both places.
case "$L" in
    *"full in ~1h2"[5-9]m*|*"full in ~1h3"[0-5]m*) ok "full in ~1h30m, within five minutes" ;;
    *) bad "full in ~1h30m, within five minutes" "$L" ;;
esac
has "the ctx segment it is appended to is untouched" "$L" "ctx 1k"

# THE POSITIVE CONTROL FOR EVERY ABSENCE BELOW. The line above proves this code path renders a
# percentage, a rate and a projection when the client supplies the windows; a later case
# asserting that one of them is missing is therefore about the input and not about a segment
# that never renders at all.
has "positive control: the segment renders at all" "$L" "41%"

# ==========================================================================================
echo
echo "when it clears comes first, and the windows are told apart by position"
# ==========================================================================================
# THE LABELS ARE GONE ON PURPOSE. Once a reset time is on screen it says which window it is —
# 2h27m cannot be the 7-day one — so "5h"/"7d" were four columns repeating it. Position is the
# identity instead, which makes their ORDER load-bearing and therefore asserted.
rm -f "$SAMPLES"
L="$(feed "$EPOCH" 38 $((EPOCH + 8820)) 22 $((EPOCH + 594000)) | strip)"
has   "each window renders as its reset then its percentage, 5-hour first" \
      "$L" "· 2h27m 38% · 6d21h 22%"
hasnt "the 5-hour label is not printed" "$L" "5h "
hasnt "nor the 7-day one"               "$L" "7d "

# ==========================================================================================
echo
echo "the reset — a window that empties before it fills"
# ==========================================================================================
# The same climb, but the window resets in eighty minutes, which is sooner than the ninety it
# would take to fill. The limit will not bite, so there is nothing to say that the reset time
# does not already say, and the line says nothing.
rm -f "$SAMPLES"
R=$((EPOCH + 600 + 4800))
feed "$EPOCH"            60 "$R" 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 300))"  62 "$R" 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 64 "$R" 41 $((EPOCH + 600000)) | strip)"
has  "the reset time alone"                "$L" "· 1h20m 64%"
hasnt "and no warning is raised"           "$L" "full in"
hasnt "nor the rate, which is not actionable when the window empties first" "$L" "%/h"
hasnt "and never an invented infinity"     "$L" "∞"

# ==========================================================================================
echo
echo "no rate — a flat window projects nothing at all"
# ==========================================================================================
rm -f "$SAMPLES"
feed "$EPOCH"           60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 300))" 60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
has   "the percentage is still shown"     "$L" "60%"
hasnt "no fill projection"                "$L" "full in"
hasnt "and never an infinity"             "$L" "∞"

# A single sample cannot have a slope, and that must render as nothing rather than as 0m.
rm -f "$SAMPLES"
L="$(feed "$EPOCH" 60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
has   "one sample still shows the percentage" "$L" "60%"
hasnt "one sample projects nothing"           "$L" "full in"
hasnt "and does not project zero minutes"     "$L" "~0m"

# ==========================================================================================
echo
echo "the floors — a span too short, and a move too small, measure nothing"
# ==========================================================================================
# BELOW EITHER FLOOR ONE ROUNDING STEP IS THE WHOLE SIGNAL. A percentage arrives rounded to a
# whole point, so across five minutes or across a single point the quantum dominates whatever
# is actually being measured, and the ETA that comes out of it is noise with a number's face
# on. Saying nothing there is the honest report that the meter cannot yet tell.
rm -f "$SAMPLES"
feed "$EPOCH" 60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 300))" 64 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
hasnt "five minutes of history projects nothing, however fast it climbed" "$L" "full in"
has   "the percentage is still rendered"                                  "$L" "64%"
# THE POSITIVE CONTROL: the same climb, carried past ten minutes, does project. Without it the
# case above passes just as well against a meter that never projects anything at all.
L="$(feed "$((EPOCH + 600))" 68 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
has "ten minutes of the same climb does project" "$L" "full in ~40m"

# Ten minutes is long enough; one point of movement is not.
rm -f "$SAMPLES"
feed "$EPOCH" 60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 61 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
hasnt "a single point across ten minutes projects nothing" "$L" "full in"
rm -f "$SAMPLES"
feed "$EPOCH" 60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 62 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
has "two points across the same span does" "$L" "full in ~3h1"

# ==========================================================================================
echo
echo "a reset mid-history is a boundary, not a fall in usage"
# ==========================================================================================
# 88% then 90%, then the window turns over to 3% and climbs to 5%. Measured across the drop the
# rate is hugely negative and nothing would be projected at exactly the moment a fresh window
# starts filling; measured from the drop it is 0.4 points a minute again, so 93 points take
# almost four hours.
rm -f "$SAMPLES"
feed "$EPOCH"            88 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 300))"  90 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 600))"   3 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 900))"   5 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 1200))" 7 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
has "the new window projects from after the reset" "$L" "↑24.0%/h"
has "and the fill is measured from its own level"  "$L" "full in ~3h52m"

# ==========================================================================================
echo
echo "a full window — nothing left to project, so the reset is the whole story"
# ==========================================================================================
rm -f "$SAMPLES"
R=$((EPOCH + 2820))
feed "$EPOCH"           98 "$R" 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 300))" 100 "$R" 41 $((EPOCH + 600000)) | strip)"
has   "when it empties, beside the fact that it is full" "$L" "· 42m 100%"
hasnt "and it is not reported as still filling"          "$L" "full in"
E="$(meter env "$((EPOCH + 300))")"
is "a full window is zero seconds from full, which is a measurement" \
   "0" "$(val SP_LIMIT_5H_ETA <<<"$E")"

# ==========================================================================================
echo
echo "an absent window renders nothing, never 0%"
# ==========================================================================================
rm -f "$SAMPLES"
L="$(feed "$EPOCH" - - - - | strip)"
is    "the whole segment is gone when the client sent no rate_limits" \
      "" "$(printf '%s' "$L" | sed -n 's/.*\(· [0-9hdm]* [0-9]*%\).*/\1/p')"
hasnt "and above all not a reassuring zero"         "$L" "0%"
has   "the ctx segment is still rendered"           "$L" "ctx 1k"
is    "and no sample file is written"               "absent" \
      "$([ -e "$SAMPLES" ] && echo present || echo absent)"

# One window present and one absent — they are independently absent, and the present one must
# still render.
rm -f "$SAMPLES"
L="$(feed "$EPOCH" 55 $((EPOCH + 8820)) - - | strip)"
has "the window that was sent renders"   "$L" "· 2h27m 55%"
is  "and it is the only pair on the line" "1" \
    "$(printf '%s' "$L" | grep -o '· [0-9hdm]* [0-9]*%' | wc -l)"

# ==========================================================================================
echo
echo "the samples file — pruned to six hours, and written whole"
# ==========================================================================================
# TWO HUNDRED LINES THREE MINUTES APART is ten hours, so all but the trailing six are older
# than the window. They are given distinct percentages, because a run of identical readings is
# collapsed on purpose and would make this case pass for the wrong reason.
rm -f "$SAMPLES"
python3 - "$SAMPLES" "$EPOCH" <<'PY'
import sys
path, epoch = sys.argv[1], int(sys.argv[2])
with open(path, "w") as fh:
    for i in range(200):
        fh.write("%d %d %d %d %d\n" % (epoch - (200 - i) * 180, i % 100, epoch + 86400,
                                       i % 100, epoch + 600000))
PY
is "planted" "200" "$(wc -l < "$SAMPLES")"
feed "$EPOCH" 64 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
# The hundred and twenty within the six hours, plus the one just written.
is "pruned to the trailing six hours" "121" "$(wc -l < "$SAMPLES")"
OLDEST="$(head -1 "$SAMPLES" | cut -d' ' -f1)"
[ "$((EPOCH - OLDEST))" -le 21600 ] \
    && ok "nothing older than the window survives" \
    || bad "nothing older than the window survives" "oldest is $((EPOCH - OLDEST))s old"

# A run of identical readings is kept as its two endpoints, so a flat account does not grow the
# file without bound and does not read as stale either.
rm -f "$SAMPLES"
for i in 0 60 120 180 240; do
    feed "$((EPOCH + i))" 64 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
done
is "a run of identical samples collapses to its endpoints" "2" "$(wc -l < "$SAMPLES")"
is "and the last endpoint is the newest reading" "$((EPOCH + 240))" \
   "$(tail -1 "$SAMPLES" | cut -d' ' -f1)"

# ==========================================================================================
echo
echo "env — what the collector reads, with no hook of its own"
# ==========================================================================================
rm -f "$SAMPLES"
feed "$EPOCH"           60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 300))" 62 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 600))" 64 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
E="$(meter env "$((EPOCH + 660))")"
is "the 5-hour percentage is the newest sample's" "64" "$(val SP_LIMIT_5H_PCT <<<"$E")"
is "the 7-day percentage too"                     "41" "$(val SP_LIMIT_7D_PCT <<<"$E")"
is "the age is measured from that sample"         "60" "$(val SP_LIMIT_AGE   <<<"$E")"
# 36 points at 0.4 a minute is 90 minutes, published in seconds.
between "the projection is published in seconds" 5100 5700 "$(val SP_LIMIT_5H_ETA <<<"$E")"
is "a flat window publishes no projection, not zero" "-" "$(val SP_LIMIT_7D_ETA <<<"$E")"

# NO SAMPLE IS `-`, NOT 0. The positive control is the block above: the same keys carried real
# numbers a moment ago, so this is about the file being empty and not about keys that never
# render.
rm -f "$SAMPLES"
E="$(meter env "$EPOCH")"
is "no sample yet reads -"          "-" "$(val SP_LIMIT_5H_PCT <<<"$E")"
is "and its projection reads - too" "-" "$(val SP_LIMIT_5H_ETA <<<"$E")"
is "and the age reads -"            "-" "$(val SP_LIMIT_AGE    <<<"$E")"

# A BROKEN READ IS `?`, WHICH IS A DIFFERENT FACT FROM `-`. A pane that renders a corrupt file
# as "nothing recorded" has turned a fault into calm.
printf 'this is not a sample\n' > "$SAMPLES"
E="$(meter env "$EPOCH")"
is "a corrupt sample file reads ?"      "?" "$(val SP_LIMIT_5H_PCT <<<"$E")"
is "its projection reads ?"             "?" "$(val SP_LIMIT_5H_ETA <<<"$E")"
is "the 7-day window reads ? as well"   "?" "$(val SP_LIMIT_7D_PCT <<<"$E")"
is "and the age reads ?"                "?" "$(val SP_LIMIT_AGE    <<<"$E")"

# A line-mode run repairs it from the hook, which is authoritative — otherwise one bad write
# leaves the collector answering `?` forever with nothing able to clear it.
feed "$EPOCH" 64 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
is "and a live hook repairs it" "64" "$(val SP_LIMIT_5H_PCT <<<"$(meter env "$EPOCH")")"

# `env` MUST NOT READ STDIN. It runs from a collector where stdin is whatever the parent left
# open, and a read on an inherited terminal blocks forever — which presents as a collector that
# simply stopped writing its snapshot.
E="$(blob 99 $((EPOCH + 86400)) 99 $((EPOCH + 600000)) "$TP" | meter env "$EPOCH")"
is "env ignores a blob on stdin and answers from the samples" \
   "64" "$(val SP_LIMIT_5H_PCT <<<"$E")"

# ==========================================================================================
echo
echo "the colours — each window banded by its own value"
# ==========================================================================================
# ASSERTED AGAINST THE ESCAPES, NOT A STRIPPED LINE. Colour is the whole of the signal here: a
# window at 95% and one at 5% read identically once the escapes are gone, so a suite that only
# ever strips them cannot tell a banded line from an unbanded one. Every case below therefore
# uses the raw line, and the green case is the positive control for the two that follow — it
# proves the band moves with the value rather than being one constant colour.
ESC=$'\033'
rm -f "$SAMPLES"
L="$(feed "$EPOCH" 36 $((EPOCH + 8820)) 13 $((EPOCH + 600000)))"
has "under the warn band the window is green" "$L" "${ESC}[32m36%"
has "and its reset time is dim beside it"     "$L" "${ESC}[2m2h27m${ESC}[0m"
rm -f "$SAMPLES"
L="$(feed "$EPOCH" 75 $((EPOCH + 8820)) 13 $((EPOCH + 600000)))"
has "at three quarters it is yellow"          "$L" "${ESC}[33m75%"
rm -f "$SAMPLES"
L="$(feed "$EPOCH" 95 $((EPOCH + 8820)) 13 $((EPOCH + 600000)))"
has "past the high band it is red"            "$L" "${ESC}[31m95%"
has "and the other window keeps its own band" "$L" "${ESC}[32m13%"

# THE WARNING INHERITS RED ONLY WHEN IT IS SOON. An hour is the threshold because that is when
# it changes what to do next; a fill four hours away is information, not an alarm, and rendering
# both the same spends the colour that makes the urgent one legible.
rm -f "$SAMPLES"
feed "$EPOCH"           10 $((EPOCH + 86400)) 13 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 30 $((EPOCH + 86400)) 13 $((EPOCH + 600000)))"
has "a fill within the hour is red" "$L" "${ESC}[31mfull in ~"
rm -f "$SAMPLES"
feed "$EPOCH"           60 $((EPOCH + 86400)) 13 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 300))" 62 $((EPOCH + 86400)) 13 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 64 $((EPOCH + 86400)) 13 $((EPOCH + 600000)))"
has "a fill further off is not"     "$L" "${ESC}[2mfull in ~1h3"

# ==========================================================================================
echo
echo "a malformed window is an absent one, not a zero"
# ==========================================================================================
# rate_limits ITSELF PRESENT, ITS WINDOWS UNREADABLE. The absence case earlier omits the key
# entirely, which the shape check refuses before either window is looked at; this is the other
# door into the same rendering, and it is the one a client reaches by changing a field's type.
# A percentage that is a string, out of range, or a bool must not become a number here.
rawblob() {
    python3 - "$1" "$TP" <<'PY2'
import json, sys
print(json.dumps({"session_id": "s-1", "transcript_path": sys.argv[2], "cwd": "/tmp",
                  "context_window": {"total_input_tokens": 1200, "used_percentage": 1,
                                     "context_window_size": 1000000},
                  "rate_limits": json.loads(sys.argv[1])}))
PY2
}
# The positive control: the same helper, with the windows well formed, does render.
rm -f "$SAMPLES"
L="$(rawblob '{"five_hour":{"used_percentage":44,"resets_at":9999999999},
               "seven_day":{"used_percentage":12,"resets_at":9999999999}}' \
     | meter line "$EPOCH" | strip)"
has "positive control: a well-formed pair renders" "$L" "44%"

for BAD in '{"five_hour":{"used_percentage":"lots","resets_at":9999999999},"seven_day":{}}' \
           '{"five_hour":{"used_percentage":140,"resets_at":9999999999},"seven_day":null}' \
           '{"five_hour":{"used_percentage":true,"resets_at":9999999999},"seven_day":[]}' \
           '{}'; do
    rm -f "$SAMPLES"
    L="$(rawblob "$BAD" | meter line "$EPOCH" | strip)"
    is "no segment for $BAD" "" \
       "$(printf '%s' "$L" | sed -n 's/.*\(· [0-9hdm]* [0-9]*%\).*/\1/p')"
    hasnt "and no reassuring zero for it" "$L" "0%"
done
is "and nothing unreadable was ever recorded as a sample" "absent" \
   "$([ -e "$SAMPLES" ] && echo present || echo absent)"

# ==========================================================================================
echo
echo "durations render at every scale, including the round ones"
# ==========================================================================================
# A WHOLE NUMBER OF HOURS IS ITS OWN CASE. Every projection here lands on a duration with
# minutes in it — 1h30m, 1h20m, 3h52m — and that is precisely the shape under which a format
# string that is never applied cannot be seen. An exact hour took the other arm and rendered
# the literal "%dh" into the status line.
#
# Driven through the reset time, which comes straight from the client's resets_at and can
# therefore be placed on any boundary asked for. The 7-day window is omitted so there is
# exactly one pair on the line to read the duration out of.
dur() {                 # dur <minutes-to-reset> -> what the line calls that duration
    rm -f "$SAMPLES"
    feed "$EPOCH" 100 $((EPOCH + $1 * 60)) - - | strip \
        | sed -n 's/.*· \([^ ]*\) 100%.*/\1/p'
}
is "minutes alone under an hour"                "47m"    "$(dur 47)"
is "an exact hour is formatted, not a literal"  "1h"     "$(dur 60)"
is "hours and minutes together"                 "1h30m"  "$(dur 90)"
is "an exact two hours"                         "2h"     "$(dur 120)"
# A 7-DAY WINDOW IS DAYS OUT FOR MOST OF ITS LIFE, so days keep their hours: "6d" would throw
# away the twenty-one that decide whether the day finishes.
is "days keep their hours"                      "6d21h"  "$(dur 9900)"
is "and drop them when there are none"          "3d"     "$(dur 4320)"
hasnt "and no unexpanded format ever reaches the line" \
      "$(dur 60)$(dur 120)$(dur 4320)$(dur 9900)" "%d"

# ==========================================================================================
echo
echo "the line stays readable"
# ==========================================================================================
# MEASURED WITH THE PROJECTION ON SCREEN, which is the longest the line ever gets.
rm -f "$SAMPLES"
feed "$EPOCH"           60 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
feed "$((EPOCH + 300))" 62 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) >/dev/null
L="$(feed "$((EPOCH + 600))" 64 $((EPOCH + 86400)) 41 $((EPOCH + 600000)) | strip)"
has "the projection is on the line being measured" "$L" "full in"
W="$(printf '%s' "$L" | wc -m)"
[ "$W" -le 110 ] && ok "under 110 columns ($W)" || bad "under 110 columns" "$W"

echo
tl_summary
