#!/usr/bin/env bash
#
# test-skuld.sh — skuld is a persona with a lane of its own, a partition no other persona
# matches, a drafting brief, and tools that cannot file what it drafts.
#
# tier: T1
# covers: spira/runes/skuld.* spira/conf.d/SPIRA_SKULD_LABEL spira/conf.d/SPIRA_LANES spira/bead.sh sentinel/src/*.rs spira/testlib/complete.toml spira-config/src/*.rs spira-world/src/*
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

echo; echo "FILE: bead.sh file --for skuld stamps the draft partition"
cat > "$T/stub-bd" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    create) printf '%s\n' "$*" >> "$BD_LOG_PATH"; echo sp-n1 ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$T/stub-bd"; printf '#!/usr/bin/env bash\nexit 0\n' > "$T/stub-lc"; chmod +x "$T/stub-lc"
echo "testrepo | $T/testrepo | push | origin/main | | true | plan" > "$T/repos.map"
tl_config SPIRA_BD="$T/stub-bd" SPIRA_DB="$T/db" SPIRA_REPO_MAP="$T/repos.map" SPIRA_RUN="$T/run"
: > "$T/bd.log"
fid="$(env -i HOME="$T" PATH="$HERE:${TOOLS:+$TOOLS:}/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$T/none.conf" \
    BD_LOG_PATH="$T/bd.log" SPIRA_LC_BIN="$T/stub-lc" SPIRA_HOME="$HERE" SPIRA_REPO="$T" \
    bead.sh file "draft a thing" --for skuld --repo testrepo --body-file /dev/null 2>&1)"
is "bead.sh file --for skuld files a bead" "sp-n1" "$fid"
want "the filed bead carries the skuld partition" "fixdraft" "$(cat "$T/bd.log")"
want "the filed bead carries the scope label" "fixscope" "$(cat "$T/bd.log")"

echo; echo "SELECT: spira-claim picks the draft request for skuld and never for the builder"
cat > "$T/beads.json" <<JSON
[{"id":"sp-d1","status":"open","issue_type":"task","priority":2,"labels":["fixscope","fixdraft"]},
 {"id":"sp-p1","status":"open","issue_type":"task","priority":2,"labels":["fixscope","plan"]}]
JSON
cat > "$T/fake-bd" <<SH
#!/bin/sh
echo "\$*" >> "$T/bd.args"
case " \$* " in *" list "*) cat "$T/beads.json" ;; *) echo '[]' ;; esac
SH
chmod +x "$T/fake-bd"; lc_mirror_bd "$T/lc"
tl_config SPIRA_BD="$T/fake-bd"
ready_set() {   # ready_set <rune> -> the rows spira-claim's ready set for that rune holds
    env -i HOME="$T" PATH="$T/lc:$HERE:${TOOLS:+$TOOLS:}/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$T/none.conf" \
        SPIRA_HOME="$HERE" SPIRA_RUNES_DIR="$HERE/runes" SPIRA_BD="$T/fake-bd" SPIRA_DB=/fake/db \
        spira-claim rune-ready "$1" --json 2>&1
}
want   "skuld's ready set holds the draft request" "sp-d1" "$(ready_set skuld)"
nowant "skuld's ready set holds no plan bead" "sp-p1" "$(ready_set skuld)"
want   "positive control: the builder's ready set holds the plan bead" "sp-p1" "$(ready_set builder)"
nowant "the builder's ready set never holds the draft request" "sp-d1" "$(ready_set builder)"

echo; echo "LANE: its own lane, two at a time, declared, and counted by dwarves.sh"
is "skuld lane" "skuld" "$(rune_get skuld RUNE_LANE)"
is "two designs draft at once" "2" "$(rune_get skuld RUNE_MAX_CONCURRENT)"
want "the lane is declared" "skuld" "$(cat "$HERE/conf.d/SPIRA_LANES")"
want "spira_lane_runes lists skuld" "skuld" "$(spira_lane_runes)"
nowant "spira_task_runes does not list skuld" "skuld" "$(spira_task_runes)"
dw="$(env -i HOME="$T" PATH="$HERE:${TOOLS:+$TOOLS:}/usr/bin:/bin" SPIRA_TOML="$SPIRA_TOML" SPIRA_CONF="$T/none.conf" SPIRA_HOME="$HERE" dwarves.sh 2>&1)"
laneline="$(printf '%s\n' "$dw" | grep -E '^  lanes +[0-9]+ +\(' || true)"
want "dwarves.sh lane report counts skuld at its concurrency" "skuld=2" "${laneline:-no lane line in dwarves.sh output: $dw}"
nowant "positive control: the lane report omits a lane that is not declared" "nosuchlane=" "$laneline"
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
rune_tools_n="$(rune_get skuld RUNE_TOOLS | tr ',' '\n' | sort)"
toml_tools_n="$(printf '%s\n' "$persona_tools" | sed -n 's/^ *"\(.*\)",$/\1/p' | sort)"
is "persona.skuld.tools agrees with the rune's RUNE_TOOLS" "$rune_tools_n" "$toml_tools_n"
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
