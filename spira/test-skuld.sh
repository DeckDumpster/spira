#!/usr/bin/env bash
#
# test-skuld.sh — skuld is a persona with a lane of its own, a partition no other persona
# matches, a drafting brief, and tools that cannot file what it drafts.
#
# tier: T1
# covers: spira/runes/skuld.* spira/conf.d/SPIRA_SKULD_LABEL spira/conf.d/SPIRA_LANES spira/bead.sh sentinel/src/*.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
TOOLS="$(command -v spira-config)" && TOOLS="$(dirname "$TOOLS")"
. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
BRIEF="$HERE/runes/skuld.md"

# Non-default scope and label, so a literal written into the rune fails here.
tl_config SPIRA_RUNES_DIR="$HERE/runes" SPIRA_SCOPE_LABEL=fixscope SPIRA_SKULD_LABEL=fixdraft
export SPIRA_HOME="$HERE" SPIRA_CONF="$T/none.conf"
. "$HERE/lib.sh"

echo "test-skuld.sh"

echo; echo "CONTRACT: bead.sh contract lists skuld with its partition"
out="$(env -i HOME="$T" PATH="$HERE:${TOOLS:+$TOOLS:}/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_CONF="$T/none.conf" SPIRA_HOME="$HERE" bead.sh contract 2>&1)"
want "contract lists skuld" "$(printf '  %-14s %s' skuld 'fixscope,fixdraft')" "$out"
want "contract still lists builder (positive control)" "  builder" "$out"

echo; echo "PARTITION: a draft request is claimable only by the skuld lane"
bead_labels="fixscope fixdraft"
claims() {   # claims <persona> -> 0 when every RUNE_LABELS label is on the fixture bead
    local l p=1
    for l in $(rune_get "$1" RUNE_LABELS | tr ',' ' '); do
        case " $bead_labels " in *" $l "*) ;; *) p=0 ;; esac
    done
    [ "$p" = 1 ] && [ -n "$(rune_get "$1" RUNE_LABELS)" ]
}
claims skuld && ok "skuld claims the fixture draft request" || bad "skuld claims the fixture draft request" "predicate does not match"
for p in $(ls "$HERE/runes" | sed -n 's/\.rune$//p' | grep -vx skuld); do
    claims "$p" && bad "$p must not claim a draft request" "predicate matches" || ok "$p does not claim a draft request"
done
bead_labels="fixscope plan"
claims skuld && bad "skuld must not claim plan work" "predicate matches" || ok "skuld does not claim plan work"

echo; echo "LANE: its own lane, two at a time, declared, and counted by dwarves.sh"
is "skuld lane" "skuld" "$(rune_get skuld RUNE_LANE)"
is "two designs draft at once" "2" "$(rune_get skuld RUNE_MAX_CONCURRENT)"
want "the lane is declared" "skuld" "$(cat "$HERE/conf.d/SPIRA_LANES")"
want "spira_lane_runes lists skuld" "skuld" "$(spira_lane_runes)"
nowant "spira_task_runes does not list skuld" "skuld" "$(spira_task_runes)"
dw="$(env -i HOME="$T" PATH="$HERE:${TOOLS:+$TOOLS:}/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$T/none.conf" SPIRA_HOME="$HERE" dwarves.sh 2>&1)"
want "dwarves.sh names the skuld lane" "skuld" "$dw"
[ -f "$HERE/conf.d/SPIRA_SKULD_LABEL" ] && ok "the label is a registered key" || bad "the label is a registered key" "no conf.d/SPIRA_SKULD_LABEL"

echo; echo "MODEL: Opus, in the rune and in the live-config fixture"
is "rune model" "claude-opus-5-5" "$(rune_get skuld RUNE_MODEL)"
want "persona entry agrees" 'model = "claude-opus-5-5"' "$(sed -n '/^\[persona.skuld\]/,/^$/p' "$HERE/testlib/complete.toml")"

echo; echo "TOOLS: skuld reads and drafts; it cannot edit, write or file"
tools=",$(rune_get skuld RUNE_TOOLS)"
for t in Edit Write; do lack "skuld has no $t" ",$t," "$tools,"; done
for t in bead.sh "bd " "git commit" "git pu""sh"; do lack "skuld has no $t grant" "$t" "$tools"; done
lack "skuld has no bare Bash" ",Bash," "$tools"
want "skuld can note on its bead" "Bash(work note*)" "$tools"
want "skuld can read the code" "Bash(git -C * show*)" "$tools"
persona_tools="$(sed -n '/^\[persona.skuld\]/,/^\[persona.skuld.lease\]/p' "$HERE/testlib/complete.toml")"
for t in Edit Write bead.sh; do lack "persona.skuld.tools has no $t" "\"$t" "$persona_tools"; done
want "persona.skuld.tools carries work note" "Bash(work note*)" "$persona_tools"
want "positive control: the builder does have Edit" "Edit" "$(rune_get builder RUNE_TOOLS)"
want "positive control: the matcher sees a bead.sh grant" "bead.sh" "$(rune_get warden RUNE_TOOLS)"

echo; echo "BRIEF: drafts as notes, never files"
for s in "Read the code the change will touch" "Check every mechanism you name against the law" "### Draft: <title>" "## Outcome" "## Build" "## Acceptance"; do
    want "the brief names: $s" "$s" "$(cat "$BRIEF")"
done
want "the brief leaves drafts as notes" "work note" "$(cat "$BRIEF")"
want "the brief exits through work done" "work done" "$(cat "$BRIEF")"

tl_summary
