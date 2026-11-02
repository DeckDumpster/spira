#!/usr/bin/env bash
# rc-cutover-mode.sh <repo> <landed> <unclaimed> <abandoned> [final] — one move of the cutover-mode world,
# run again and again: once <landed> and <abandoned> are CERTIFIED, walk the rounds runbook's
# checked entries in the order a cutover meets them — pause, drain, enter cutover mode, hand-land
# <landed>, clear its lease, hold the window open while <unclaimed> is claimable, leave, roll back
# by abandoning a round of <abandoned> — then check the world is halted again with no stop of
# cutover mode's left. Exit 3 for a move that has to wait, 9 for a wrong answer, 0 for a move made.
# With `final`, only that last check, and a walk not yet done is a wrong answer.
set -u
repo="$1" landed="$2" unclaimed="$3" abandoned="$4" final="${5:-}"
here="spira/sim/scenarios/steps"
phase_file="$SIM_WORLD/cutover.phase"
phase=$(cat "$phase_file" 2>/dev/null || echo wait)
fail() { echo "rc-cutover-mode: $phase: $1" >&2; exit 9; }
next() { echo "$1" > "$phase_file"; phase="$1"; }
state_of() {  # state_of <bead>
    env -u SPIRA_DB -u SPIRA_LC_SOCKET -u SPIRA_LC_PASSWORD_FILE sim probe "$SIM_WORLD" | tr -d ' ' | grep "\"bead\":\"$1\"" | grep -o '"lc_state":"[A-Z_]*"' | cut -d'"' -f4
}
book() {  # book <entry> [NAME=value ...] — that runbook entry, with this world's names
    REPO="$repo" REV=sim-rev WHY="cutover drill" bash "$here/runbook.sh" "$@"
}

# world stop and world status ask systemd for the timers they act on; the world has no user bus, so
# this answers every unit name as known and does nothing.
export SPIRA_SYSTEMCTL="$SIM_WORLD/systemctl-stub"
if [ ! -x "$SPIRA_SYSTEMCTL" ]; then
    printf '#!/bin/sh\n[ "$2" = list-unit-files ] && case "$3" in *"*"*) ;; *) echo "$3 enabled" ;; esac\nexit 0\n' > "$SPIRA_SYSTEMCTL" && chmod +x "$SPIRA_SYSTEMCTL" || exit 9
fi
# round-recert finds its release from where its own executable is; the world's release links its
# executables out of the build tree, so a release of links with a real round-recert in bin/ stands
# in for an installed one, and the world's own round-vm for the VM.
rel="$SIM_WORLD/recert-release"
if [ ! -x "$rel/bin/round-recert" ]; then
    on_path=$(command -v round-recert) || exit 9
    src=$(dirname "$(dirname "$on_path")")
    [ -d "$src/spira" ] || exit 9
    mkdir -p "$rel/bin" || exit 9
    for f in "$src"/*; do [ "${f##*/}" = bin ] || ln -sfn "$(readlink -f "$f")" "$rel/${f##*/}" || exit 9; done
    for f in "$src"/bin/*; do
        n=${f##*/}
        ln -sfn "$(command -v "$n" || readlink -f "$f")" "$rel/bin/$n" || exit 9
    done
    vm=$(command -v round-vm) || exit 9
    rm -f "$rel/bin/round-vm" || exit 9
    printf '#!/bin/sh\nSIM_WORLD=%s exec %s "$@"\n' "'$SIM_WORLD'" "'$vm'" > "$rel/bin/round-vm" && chmod +x "$rel/bin/round-vm" || exit 9
    rm -f "$rel/bin/round-recert" && cp "$(readlink -f "$src/bin/round-recert")" "$rel/bin/round-recert" || exit 9
fi
export PATH="$rel/bin:$PATH"
[ -z "$final" ] || [ "$phase" = done ] || fail "the walk never finished"

while :; do
    case "$phase" in
    wait)
        [ "$(state_of "$landed")" = CERTIFIED ] && [ "$(state_of "$abandoned")" = CERTIFIED ] || exit 3
        next pause ;;
    pause) book pause-rounds || exit $?; next drain ;;
    drain) book drain-rounds || exit $?; next enter ;;
    enter)
        book cutover-enter || exit $?
        [ -f "$SPIRA_RUN/world.cutover-mode" ] && [ ! -f "$SPIRA_RUN/world.halted" ] || fail "cutover mode left no record, or the stamp in place"
        world unpause --reserve "$unclaimed" || fail "could not lift the reservation on $unclaimed"
        next land ;;
    land) book hand-land BATCH=rc-cm BEAD="$landed" || exit $?; next lease ;;
    lease) book clear-lease BATCH=rc-cm || exit $?; next check ;;
    check) book cutover-check || exit $?; next hold-1 ;;
    hold-*)
        # Each hold is one move apart, so the sentinel's calls meet a claimable bead in the window.
        [ "$(state_of "$unclaimed")" = READY ] || fail "$unclaimed was claimed under cutover mode: $(state_of "$unclaimed")"
        n=${phase#hold-}
        if [ "$n" -lt 4 ]; then next "hold-$((n + 1))"; else next leave; fi
        exit 0 ;;
    leave)
        book cutover-leave || exit $?
        [ -f "$SPIRA_RUN/world.halted" ] && [ ! -f "$SPIRA_RUN/world.cutover-mode" ] || fail "world stop did not re-halt"
        if world status | grep -q 'owner=cutover-mode'; then fail "a stop of cutover mode's outlived world stop"; fi
        queue round open "$repo" --members "$abandoned" --name rc-ab || fail "no round of $abandoned to roll back"
        next rollback ;;
    rollback) book rollback BATCH=rc-ab || exit $?; next done ;;
    done)
        [ -f "$SPIRA_RUN/world.halted" ] || fail "the world is not halted"
        [ "$(state_of "$unclaimed")" = READY ] || fail "$unclaimed was claimed: $(state_of "$unclaimed")"
        exit 0 ;;
    *) fail "unknown phase" ;;
    esac
done
