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

# done_after_ask <brief> — prints each `work done` command that follows an ask command outside
# the paragraph that opens "**Your finding is that no decision is needed**". An ask holds its
# bead on the operator's reply, so that close is refused.
done_after_ask() {
    awk '/^## / { asked = 0; noask = 0 }
         /^\*\*Your finding asks/ { noask = 0 }
         /^\*\*Your finding is that no decision/ { noask = 1 }
         /^    .*\{\{ASK\}\}/ { asked = 1 }
         /^    work done( |$)/ && asked && !noask { print NR": "$0 }' "$1"
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
echo "the spike names both closings and the condition that picks between them"
ask_para="$(awk '/^\*\*Your finding asks/ {p=1; print; next} /^\*\*Your finding is that/ {p=0} p' "$spike")"
noask_para="$(awk '/^\*\*Your finding is that no decision/ {p=1} /^Either way/ {p=0} p' "$spike")"
want "the ask closing says the held bead is the end state, not DONE" "not DONE" "$ask_para"
want "the ask closing names what closes it" "operator's answer closes the bead" "$ask_para"
want "the no-ask closing carries the work done command" "    work done" "$noask_para"
want "the no-ask closing is conditioned on there being nothing to approve" "nothing to approve" "$noask_para"

tl_summary
