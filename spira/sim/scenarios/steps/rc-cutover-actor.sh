#!/bin/sh
# rc-cutover-actor.sh <lane> <command...> — an actor cutover mode holds on <lane>. While the world
# is in cutover mode, the actor must say it skipped under that lane's stop; exit 9 when it ran
# instead. Outside cutover mode a land or claims actor runs as itself and any other does not run, so a
# remedy or a page of its own never moves this world.
set -u
lane="$1"
shift
rec="$SPIRA_RUN/world.cutover-mode"
held=0
[ -f "$rec" ] && held=1
[ "$held" = 1 ] || [ "$lane" = land ] || [ "$lane" = claims ] || exit 0
out=$("$@" 2>&1)
printf '%s\n' "$out"
[ "$held" = 1 ] && [ -f "$rec" ] || exit 0
[ "$lane" = claims ] && exit 0   # a claim held is seen as the bead staying READY
printf '%s\n' "$out" | grep -q "skipped — the $lane lane of " && exit 0
echo "rc-cutover-actor: $1 ran under cutover mode without naming its $lane stop" >&2
exit 9
