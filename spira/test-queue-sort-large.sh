#!/usr/bin/env bash
#
# test-queue-sort-large.sh — queue_sort_rows returns every row however large PRIO_JSON is.
#
# WHAT THIS IS FOR. batch.sh and cockpit.sh both call
#
#     PRIO_JSON="$prio_json" queue_sort_rows "$repo" "$base_sha"
#
# with the full `bd show --json` of every certified bead. On 2026-09-18 that reached
# 266 KiB at 40 beads. Linux caps one environment string at 128 KiB, so every exec inside
# the function failed E2BIG ("Argument list too long"), the python sort ran under
# 2>/dev/null, and it emitted ZERO rows. batch.sh then cut no batch and the ops pane showed
# an empty queue, for nine hours, with 40 branches certified and waiting (sp-m5iq3).
#
# A cliff with positive feedback: it works below the threshold, and the stall it causes is
# what pushes the backlog further past it. So the case under test is the LARGE one; a
# suite that only ever feeds a handful of rows passes against the broken code.
#
# SEEN TO FAIL against the lib.sh that preceded sp-m5iq3: case 1 returns 0 of 40 rows.

# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n' "$1"; fail=$((fail+1)); }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1: wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

. "$HERE/conf.sh"
. "$HERE/lib.sh"

# A throwaway repository, so queue_is_suite_transition has real commits to diff.
git init -q "$TMP/repo"
git -C "$TMP/repo" config user.email sort-suite@example.invalid
git -C "$TMP/repo" config user.name  "queue sort suite"
echo seed > "$TMP/repo/seed"
git -C "$TMP/repo" add seed >/dev/null 2>&1
git -C "$TMP/repo" commit -qm seed >/dev/null 2>&1
base="$(git -C "$TMP/repo" rev-parse HEAD)"

N=40
rows="$TMP/rows"
: > "$rows"
for i in $(seq 1 "$N"); do
    printf 'sp-t%03d %s %d\n' "$i" "$base" "$(( 1789700000 + i ))" >> "$rows"
done

# A PRIO_JSON shaped like real `bd show --json` output: every bead carries a long body.
# 8 KiB each x 40 = ~320 KiB, comfortably past the 128 KiB single-string limit.
big_json="$(python3 - "$N" <<'PY'
import json, sys
n = int(sys.argv[1])
print(json.dumps([{"id": "sp-t%03d" % i, "priority": i % 3,
                   "title": "bead %d" % i, "description": "x" * 8192}
                  for i in range(1, n + 1)]))
PY
)"
size=${#big_json}

# 0. POSITIVE CONTROL. The case below is only meaningful if the payload is actually past
#    the limit; a fixture that shrank under it would pass against the broken code.
[ "$size" -gt 131072 ] && ok "fixture PRIO_JSON is past the 128 KiB limit ($size bytes)" \
                       || bad "fixture PRIO_JSON is only $size bytes — not testing the cliff"

# 1. THE CLIFF. Every row must come back.
got="$(PRIO_JSON="$big_json" queue_sort_rows "$TMP/repo" "$base" < "$rows" 2>/dev/null | grep -c .)"
is "all $N rows survive a PRIO_JSON past 128 KiB" "$N" "$got"

# 2. PRIORITY IS STILL HONOURED when the payload is large. Priority 0 beads sort first.
first_prio="$(PRIO_JSON="$big_json" queue_sort_rows "$TMP/repo" "$base" < "$rows" 2>/dev/null \
              | head -1 | awk '{print $2+0}')"
is "priority still orders the rows" "0" "$first_prio"

# 3. THE SMALL CASE, so the fix is not a regression on the path that always worked.
small='[{"id":"sp-t001","priority":2},{"id":"sp-t002","priority":0}]'
got="$(printf 'sp-t001 %s 1\nsp-t002 %s 2\n' "$base" "$base" \
       | PRIO_JSON="$small" queue_sort_rows "$TMP/repo" "$base" 2>/dev/null | awk '{print $4}' | tr '\n' ' ')"
is "a small PRIO_JSON still sorts by priority" "sp-t002 sp-t001 " "$got"

# 4. FAIL OPEN. Ranking is an optimisation; if it breaks, the rows must still come out.
#    Garbage priority data must not empty the queue.
got="$(PRIO_JSON='{not json' queue_sort_rows "$TMP/repo" "$base" < "$rows" 2>/dev/null | grep -c .)"
is "unparseable PRIO_JSON still returns every row" "$N" "$got"

# 5. FAIL-OPEN WARNING. The caller must know ranking was skipped; silent degradation is the
#    shape of the original bug. Verify the warning reaches stderr.
warn="$(PRIO_JSON='{not json' queue_sort_rows "$TMP/repo" "$base" < "$rows" 2>&1 >/dev/null)"
is "fail-open emits a warning to stderr" 1 "$(printf '%s' "$warn" | grep -c 'ranking failed')"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
