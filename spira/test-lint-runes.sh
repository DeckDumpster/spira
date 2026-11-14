#!/usr/bin/env bash
#
# test-lint-runes.sh — static text checks on the persona briefs: no fixture, no database.
#
# tier: T0
# covers: spira/runes/*.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-lint-runes.sh"

# done_after_ask <brief> — prints each `work done` command that follows an ask command with
# no heading between them. An ask holds its bead on the operator's reply, so that close is refused.
done_after_ask() {
    awk '/^#/ { asked = 0 }
         /^    .*\{\{ASK\}\}/ { asked = 1 }
         /^    work done( |$)/ && asked { print NR": "$0 }' "$1"
}

echo
echo "a closing that posts an ask does not then run work done"
planted="$(mktemp)"; trap 'rm -f "$planted"' EXIT INT TERM
printf '%s\n' 'Finish:' '' '    {{ASK}} --subject "s" --kind question --class design' '' '    work done --delivers "x"' > "$planted"
want "the matcher names a work done planted after an ask" "work done" "$(done_after_ask "$planted")"

spike="$HERE/runes/spike.md"
want "spike.md still posts its design ask" "{{ASK}}" "$(cat "$spike")"
got="$(done_after_ask "$spike")"
if [ -z "$got" ]; then ok "spike.md prescribes no work done after the ask"
else bad "spike.md prescribes work done after the ask" "$got"; fi

echo
echo "the spike names both closings"
want "the ask closing ends held on the reply" "held" "$(cat "$spike")"
want "the no-ask closing ends in work done" "no ask" "$(cat "$spike")"

tl_summary
