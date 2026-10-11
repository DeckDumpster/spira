#!/usr/bin/env bash
#
# bare-suites.sh — run the bare-host suites (every `# bare-host:` suite) on this machine.
#
#   bare-suites.sh (--suites <csv|all> | --selected <csv>) [--results <dir>]
#
# The one place the bare-host selection and loop live: the publish gate's G12 step, a round's
# bare lane and bare-local.sh all call it, so they cannot drift (law-test-selection-and-plan-
# are-one-source). Each suite runs as this user, directly, because it builds its own container.
#
# --suites   the selection: `all`, or a comma list intersected with the bare-host suites.
#            An empty list runs nothing and says so. A name that is not a bare-host suite
#            is refused (exit 2): a typo must not pass by selecting nothing.
# --selected the corpus's own selection (every suite the diff chose, bare-host or not): the
#            bare-host suites among them run and the rest are ignored.
# --results  also write `<suite>.result` there in the corpus's own record format, so a
#            round counts them like any suite.
#
# EXIT  0 every selected suite passed or skipped (77) · 1 one failed · 2 usage, or no bare
#       suite exists to select from.
#
# covers: spira/bare-suites.sh spira/bare-local.sh spira/host-check.sh .github/workflows/gate.yml
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

sel="" have_sel=0 results="" strict=1
while [ $# -gt 0 ]; do
    case "$1" in
        --suites)  sel="${2-}"; have_sel=1; shift 2 || { echo "bare-suites.sh: --suites needs a value" >&2; exit 2; } ;;
        --selected) sel="${2-}"; have_sel=1; strict=0; shift 2 || { echo "bare-suites.sh: --selected needs a value" >&2; exit 2; } ;;
        --results) results="${2:?--results needs a directory}"; shift 2 ;;
        *) echo "usage: bare-suites.sh (--suites <csv|all> | --selected <csv>) [--results <dir>]" >&2; exit 2 ;;
    esac
done
[ "$have_sel" = 1 ] || { echo "usage: bare-suites.sh (--suites <csv|all> | --selected <csv>) [--results <dir>]" >&2; exit 2; }

mapfile -t all < <(bash "$HERE/host-check.sh" --bare-suites)
[ "${#all[@]}" -gt 0 ] || { echo "bare-suites: no suite is marked '# bare-host:' — nothing to select from" >&2; exit 2; }

selected=()
if [ "$sel" = all ]; then
    selected=("${all[@]}")
else
    IFS=, read -ra want <<< "$sel"
    for w in "${want[@]}"; do
        [ -n "$w" ] || continue
        found=0
        for a in "${all[@]}"; do [ "$a" = "$w" ] && found=1; done
        [ "$found" = 1 ] || [ "$strict" = 0 ] || { echo "bare-suites: '$w' is not a bare-host suite (have: ${all[*]})" >&2; exit 2; }
        [ "$found" = 0 ] || selected+=("$w")
    done
fi

for a in "${all[@]}"; do
    printed=0
    for s in "${selected[@]}"; do [ "$s" = "$a" ] && printed=1; done
    [ "$printed" = 1 ] || printf 'bare-suites: %s not selected — skipping\n' "$a"
done
[ "${#selected[@]}" -gt 0 ] || { echo "bare-suites: checked 0 suites (none selected)"; exit 0; }
[ -z "$results" ] || mkdir -p "$results"

fail=0
for s in "${selected[@]}"; do
    t0=$(date +%s)
    rc=0
    if [ -n "$results" ]; then
        bash "$HERE/$s" 2>&1 | tee "$results/$s.out"
        rc=${PIPESTATUS[0]}
    else
        bash "$HERE/$s" || rc=$?
    fi
    secs=$(( $(date +%s) - t0 ))
    case "$rc" in 0) st=ok ;; 77) st=skip ;; *) st=red; fail=1; printf 'bare-suites: %s FAILED (exit %d)\n' "$s" "$rc" >&2 ;; esac
    [ -z "$results" ] || printf '%s %s %s - - - %s\n' "$st" "$(date +%s)" "$secs" "$rc" > "$results/$s.result"
    printf 'bare-suites: %s %s (%ss)\n' "$s" "$st" "$secs"
done
printf 'bare-suites: checked %d suites\n' "${#selected[@]}"
exit "$fail"
