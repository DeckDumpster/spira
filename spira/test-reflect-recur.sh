#!/usr/bin/env bash
#
# test-reflect-recur.sh — reflect-recur.sh files a bead for a judgement-tier CHECK: only when
# its key recurs: twice inside the window files exactly one, a one-off files none, and the weekly cap holds.
# The knobs are pinned off their defaults (10 days, cap 2) so a literal in the script fails.
#
# covers: spira/reflect-recur.sh spira/reflect.sh spira/conf.d/SPIRA_REFLECT_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
H="$T/home"; mkdir -p "$H"
for f in lib.sh conf.sh suite-covers.sh conf.d conf.d.defaults.generated.sh conf.d.keys.generated.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$H/$f"
done
cp "$HERE/reflect-recur.sh" "$H/"
printf '#!/bin/sh\necho "$@" >> "%s/filed"\ncat >/dev/null\n' "$T" > "$H/bead.sh"; chmod +x "$H/bead.sh"

run() { env -i PATH="$PATH" HOME="$T" SPIRA_HOME="$H" SPIRA_RUN="$T/run" SPIRA_REFLECT_LEDGER="$T/ledger.tsv" \
    SPIRA_REFLECT_RECUR_HITS=2 SPIRA_REFLECT_RECUR_DAYS=10 SPIRA_REFLECT_RECUR_WEEKLY_CAP=2 \
    bash "$H/reflect-recur.sh" "$@" >/dev/null 2>&1; }
filed() { [ -f "$T/filed" ] && wc -l < "$T/filed" || echo 0; }
fail=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: want $3 got $2"; fail=1; fi; }
D=86400; N=2000000000

run "CHECK: sentinel/src/a.rs::alpha | nothing ready" "$N"
check "one-off files nothing" "$(filed)" 0
run "CHECK: Sentinel/src/a.rs::Alpha | other wording" "$((N + 11 * D))"
check "second hit outside the window files nothing" "$(filed)" 0
run "CHECK: sentinel/src/a.rs::alpha | again" "$((N + 15 * D))"
check "second hit inside the window files one" "$(filed)" 1
run "CHECK: sentinel/src/a.rs::alpha | and again" "$((N + 16 * D))"
check "a filed key does not file again" "$(filed)" 1

for k in b c d; do
    run "CHECK: $k | x" "$((N + 30 * D))"; run "CHECK: $k | y" "$((N + 30 * D + 1))"
done
check "two files this week, the third is held" "$(filed)" 3
run "CHECK: d | z" "$((N + 38 * D))"
check "capped key files once the week rolls" "$(filed)" 4
exit $fail
