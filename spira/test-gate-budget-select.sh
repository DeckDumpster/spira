#!/usr/bin/env bash
#
# test-gate-budget-select.sh — the per-bead gate's budgeted selector (sp-vq2za): the gate
# runs exactly SPIRA_GATE_BUDGET seconds of the most relevant suites, not the near-full
# corpus a fan-out file like spira/lib.sh used to select. Four things must be true:
#
#   1. a lib.sh-shaped diff with a 500-suite fixture selects at most the budget's worth of
#      predicted wall, the most specific (fewest # covers: tokens) suites first.
#   2. an unmeasured suite counts at its tier's budget cap, never at zero.
#   3. every candidate dropped for budget is named on stderr (fast-suites.sh's own
#      convention), and the summary line reports selected/dropped/predicted.
#   4. gate-touched.sh wires this in, and an ejected suite always survives the cut
#      (law-a-retry-must-change-an-input) because it is added back after this script runs,
#      never ranked or dropped by it.
#
# tier: T1
# covers: spira/gate-budget-select.sh spira/gate-touched.sh spira/gate.sh spira/tsd-query.sh spira/suite-covers.sh spira/tier-budget.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
SEL="$HERE/gate-budget-select.sh"

echo "test-gate-budget-select.sh"

# ---------------------------------------------------------------------------------------
# Part 1 (UC acceptance test #1): a 500-suite fixture, one glob token per rank, all
# unmeasured (no SPIRA_RUN — every cost is its T1 tier cap, 1s by default). Budget=300
# fits exactly the 300 most specific suites: token count 1 (test-s001.sh) through 300.
# ---------------------------------------------------------------------------------------
echo
echo "Part 1: 500-suite fixture, most specific first, budget caps predicted wall"
BIG="$T/big"; mkdir -p "$BIG"
i=1
while [ "$i" -le 500 ]; do
    n="$(printf '%03d' "$i")"
    tokens="spira/lib.sh"
    j=2
    while [ "$j" -le "$i" ]; do
        tokens="$tokens spira/filler-$j.sh"
        j=$((j + 1))
    done
    printf '#!/usr/bin/env bash\n# covers: %s\nexit 0\n' "$tokens" > "$BIG/test-s$n.sh"
    i=$((i + 1))
done

names="$(cd "$BIG" && ls test-*.sh)"
out1="$(printf '%s\n' "$names" | SPIRA_RUN=/nonexistent bash "$SEL" --budget-secs 300 --suite-dir "$BIG" 2>"$T/err1")"
n_sel1="$(printf '%s\n' "$out1" | grep -c .)"
is    "P1: exactly 300 of 500 fit a 300s budget at 1s each" "300" "$n_sel1"
want  "P1: the single most-specific suite is kept" "test-s001.sh" "$out1"
want  "P1: the 300th most-specific suite is kept"  "test-s300.sh" "$out1"
nowant "P1: the 301st is not"                       "test-s301.sh" "$out1"
nowant "P1: the least specific of all is dropped"   "test-s500.sh" "$out1"
want  "P1: the dropped suite is named on stderr, with its predicted cost" \
      "dropped test-s500.sh (tier=T1 predicted=1.000s over budget)" "$(cat "$T/err1")"
want  "P1: the summary line reports selected/dropped/predicted" \
      "gate-budget-select: selected 300, dropped 200, predicted 300s" "$(cat "$T/err1")"

# ---------------------------------------------------------------------------------------
# Part 2 (UC acceptance test #2): an unmeasured suite counts at its tier's cap, pinned to a
# NON-DEFAULT value so the assertion could not pass on the shipped default by accident.
# ---------------------------------------------------------------------------------------
echo
echo "Part 2: an unmeasured suite counts at its tier cap, not at zero"
SMALL="$T/small"; mkdir -p "$SMALL"
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/lib.sh spira/other.sh\nexit 0\n' \
    > "$SMALL/test-t2.sh"

out_low="$(printf 'test-t2.sh\n' | SPIRA_TIER_BUDGET_T2_MS=7000 SPIRA_RUN=/nonexistent \
    bash "$SEL" --budget-secs 6 --suite-dir "$SMALL" 2>"$T/err2low")"
is    "P2: a budget under the pinned 7s cap drops the suite" "0" "$(printf '%s\n' "$out_low" | grep -c .)"
want  "P2: the drop names the pinned cap, not a made-up cost" \
      "dropped test-t2.sh (tier=T2 predicted=7.000s over budget)" "$(cat "$T/err2low")"

out_high="$(printf 'test-t2.sh\n' | SPIRA_TIER_BUDGET_T2_MS=7000 SPIRA_RUN=/nonexistent \
    bash "$SEL" --budget-secs 7 --suite-dir "$SMALL" 2>/dev/null)"
is    "P2: a budget at exactly the pinned cap keeps it" "1" "$(printf '%s\n' "$out_high" | grep -c .)"

# ---------------------------------------------------------------------------------------
# A measured suite's P90 (not its tier cap) sets its cost — needs duckdb.
# ---------------------------------------------------------------------------------------
echo
echo "Part 3: a measured suite's predicted cost is its P90, not its tier cap"
if ! command -v duckdb >/dev/null 2>&1; then
    echo "  SKIP: duckdb not found — gate-budget-select.sh needs tsd-query.sh's query layer"
else
    RUN="$T/run"; mkdir -p "$RUN/tsd"
    FAM="$RUN/tsd/suite-timing.jsonl"; : > "$FAM"
    _row() { printf '{"ts":"2026-09-28T00:00:%02dZ","host":"h1","family":"suite-timing","suite":"%s","wall_secs":%s}\n' "$1" "$2" "$3" >> "$FAM"; }
    k=0
    for w in 2 2 2; do k=$((k + 1)); _row "$k" measured.sh "$w"; done
    printf '#!/usr/bin/env bash\n# covers: spira/lib.sh\nexit 0\n' > "$SMALL/measured.sh"

    out_m="$(printf 'measured.sh\n' | SPIRA_RUN="$RUN" bash "$SEL" --budget-secs 3 --suite-dir "$SMALL" 2>/dev/null)"
    want "P3: a measured suite well under budget on its own P90 is kept" "measured.sh" "$out_m"
    out_m_tight="$(printf 'measured.sh\n' | SPIRA_RUN="$RUN" bash "$SEL" --budget-secs 1 --suite-dir "$SMALL" 2>"$T/err3")"
    is   "P3: a budget under the measured P90 drops it" "0" "$(printf '%s\n' "$out_m_tight" | grep -c .)"
    want "P3: the drop names the measured cost (2s), not the 1s tier cap" \
         "dropped measured.sh (tier=T1 predicted=2" "$(cat "$T/err3")"
fi

# ---------------------------------------------------------------------------------------
# Tag and tier-bucket ranking: within equal specificity, a tagged suite outranks an
# untagged one; across tiers, T0/T1 outranks T2 regardless of specificity.
# ---------------------------------------------------------------------------------------
echo
echo "Part 4: tag tiebreak and tier-bucket ordering"
RANK="$T/rank"; mkdir -p "$RANK"
printf '#!/usr/bin/env bash\n# covers: spira/lib.sh UC-foo-01\nexit 0\n' > "$RANK/test-tagged.sh"
printf '#!/usr/bin/env bash\n# covers: spira/lib.sh spira/pad.sh\nexit 0\n' > "$RANK/test-untagged.sh"
out_tag="$(printf 'test-tagged.sh\ntest-untagged.sh\n' | SPIRA_RUN=/nonexistent \
    bash "$SEL" --budget-secs 1 --suite-dir "$RANK" 2>/dev/null)"
want   "P4: equal specificity — the tagged suite wins the tiebreak" "test-tagged.sh" "$out_tag"
nowant "P4: the untagged one is cut"                                "test-untagged.sh" "$out_tag"

printf '#!/usr/bin/env bash\n# tier: T0\n# covers: spira/lib.sh spira/a.sh spira/b.sh spira/c.sh spira/d.sh\nexit 0\n' \
    > "$RANK/test-t0-broad.sh"
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/lib.sh\nexit 0\n' > "$RANK/test-t2-narrow.sh"
out_bucket="$(printf 'test-t0-broad.sh\ntest-t2-narrow.sh\n' \
    | SPIRA_TIER_BUDGET_T2_MS=1000 SPIRA_RUN=/nonexistent \
      bash "$SEL" --budget-secs 1 --suite-dir "$RANK" 2>/dev/null)"
want   "P4: T0/T1 outranks T2 even when T2 is more specific" "test-t0-broad.sh"  "$out_bucket"
nowant "P4: the more-specific T2 suite still loses to the tier bucket" "test-t2-narrow.sh" "$out_bucket"

# ---------------------------------------------------------------------------------------
# Part 5 (UC acceptance test #4, integration): gate-touched.sh wires the selector in, and
# an ejected suite always survives a budget too tight for anything else.
# ---------------------------------------------------------------------------------------
echo
echo "Part 5: gate-touched.sh wiring — ejected suites always survive the budget"
R="$T/repo"; git init -q -b main "$R"; mkdir -p "$R/spira"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
printf '#!/usr/bin/env bash\n# covers: spira/changed.sh\nexit 0\n' > "$R/spira/test-a.sh"
printf '#!/usr/bin/env bash\n# covers: spira/unrelated.sh\nexit 0\n' > "$R/spira/test-eject-me.sh"
printf 'x\n' > "$R/spira/changed.sh"
git -C "$R" add -A; git -C "$R" commit -q -m base
git -C "$R" checkout -q -b br
printf 'y\n' >> "$R/spira/changed.sh"
git -C "$R" add -A; git -C "$R" commit -q -m "change changed.sh"

out5="$(cd "$R" && SPIRA_GATE_REPO="$R" SPIRA_BATCH_SUITE_DIR="$R/spira" \
    SPIRA_GATE_BUDGET=0 SPIRA_RUN=/nonexistent SPIRA_GATE_EJECTED_SUITES=test-eject-me.sh \
    bash "$HERE/gate-touched.sh" main br 2>/dev/null)"
want "P5: an ejected suite survives a budget of 0" "test-eject-me.sh" "$out5"
nowant "P5: the covering (non-ejected) suite is cut at budget=0" "test-a.sh" "$out5"

want "P5-wiring: gate-touched.sh calls gate-budget-select.sh" \
    "gate-budget-select.sh" "$(cat "$HERE/gate-touched.sh")"
want "P5-wiring: gate.sh exports SPIRA_GATE_BUDGET into the fenced gate environment" \
    'SPIRA_GATE_BUDGET="${SPIRA_GATE_BUDGET:-300}"' "$(cat "$HERE/gate.sh")"

tl_summary
