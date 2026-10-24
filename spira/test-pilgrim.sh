#!/usr/bin/env bash
#
# test-pilgrim.sh — the pilgrim is a persona with a lane of its own, a partition no other
# persona matches, Opus, and a brief that walks and files but never fixes.
#
# tier: T1
# covers: spira/chamber/pilgrim.* spira/conf.d/SPIRA_PILGRIM_LABEL spira/conf.d/SPIRA_LANES spira/bead.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
TOOLS="$(command -v spira-config)" && TOOLS="$(dirname "$TOOLS")"
. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
FAYTH="$HERE/chamber/pilgrim.fayth"
BRIEF="$HERE/chamber/pilgrim.md"

# Non-default scope and label, so a literal written into the fayth fails here.
tl_config SPIRA_CHAMBER="$HERE/chamber" SPIRA_SCOPE_LABEL=fixscope SPIRA_PILGRIM_LABEL=fixwalk
export SPIRA_HOME="$HERE" SPIRA_CONF="$T/none.conf"
. "$HERE/lib.sh"

echo "test-pilgrim.sh"

echo; echo "CONTRACT: bead.sh contract lists the pilgrim with its partition"
out="$(env -i HOME="$T" PATH="$HERE:${TOOLS:+$TOOLS:}/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF="$T/none.conf" SPIRA_HOME="$HERE" bead.sh contract 2>&1)"
want "contract lists pilgrim" "$(printf '  %-14s %s' pilgrim 'fixscope,fixwalk')" "$out"
want "contract still lists builder (positive control)" "  builder" "$out"

echo; echo "PARTITION: a pilgrimage bead is claimable only by the pilgrim lane"
bead_labels="fixscope fixwalk"
claims() {   # claims <persona> -> 0 when every FAYTH_LABELS label is on the fixture bead
    local l p=1
    for l in $(fayth_get "$1" FAYTH_LABELS | tr ',' ' '); do
        case " $bead_labels " in *" $l "*) ;; *) p=0 ;; esac
    done
    [ "$p" = 1 ] && [ -n "$(fayth_get "$1" FAYTH_LABELS)" ]
}
claims pilgrim && ok "the pilgrim claims the fixture pilgrimage bead" || bad "the pilgrim claims the fixture pilgrimage bead" "predicate does not match"
for p in $(ls "$HERE/chamber" | sed -n 's/\.fayth$//p' | grep -vx pilgrim); do
    claims "$p" && bad "$p must not claim a pilgrimage bead" "predicate matches" || ok "$p does not claim a pilgrimage bead"
done
bead_labels="fixscope plan"
claims pilgrim && bad "the pilgrim must not claim plan work" "predicate matches" || ok "the pilgrim does not claim plan work"
is "pilgrim lane" "pilgrimage" "$(fayth_get pilgrim FAYTH_LANE)"
is "one at a time" "1" "$(fayth_get pilgrim FAYTH_MAX_CONCURRENT)"
want "the lane is declared" "pilgrimage" "$(cat "$HERE/conf.d/SPIRA_LANES")"
[ -f "$HERE/conf.d/SPIRA_PILGRIM_LABEL" ] && ok "the label is a registered key" || bad "the label is a registered key" "no conf.d/SPIRA_PILGRIM_LABEL"

echo; echo "MODEL: Opus, in the fayth and in the live-config fixture"
is "fayth model" "claude-opus-5-5" "$(fayth_get pilgrim FAYTH_MODEL)"
want "persona entry agrees" 'model = "claude-opus-5-5"' "$(sed -n '/^\[persona.pilgrim\]/,/^$/p' "$HERE/../spira-config/tests/fixtures/complete.toml")"

echo; echo "BRIEF: walks and files, never fixes, never the operator's tenant"
for s in "Wait for the deploy" "Sign in" "Do the user path" "File each defect" "Release what the walk created" "Exit"; do
    want "the walk names: $s" "$s" "$(cat "$BRIEF")"
done
want "the brief files through the contract" "work file" "$(cat "$BRIEF")"
want "the brief asks for a failing-first test" "fails first" "$(cat "$BRIEF")"
want "the brief never asks mid-walk" "Never ask the operator anything mid-walk" "$(cat "$BRIEF")"
want "the brief refuses the operator's tenant" "never the operator's tenant" "$(cat "$BRIEF")"
lack "the pilgrim has no Edit" ",Edit" ",$(fayth_get pilgrim FAYTH_TOOLS)"
lack "the pilgrim has no Write" ",Write" ",$(fayth_get pilgrim FAYTH_TOOLS)"
want "positive control: the builder does have Edit" "Edit" "$(fayth_get builder FAYTH_TOOLS)"

tl_summary
