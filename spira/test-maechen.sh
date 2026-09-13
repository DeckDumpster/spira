#!/usr/bin/env bash
#
# test-maechen.sh — maechen.fayth and chamber/maechen.md: the retrospective persona
#   and its brief encode the five-step pass, the three-occurrence threshold, the
#   four-property output bar, and the per-pass output bound.
#
#   ./test-maechen.sh
#
# WHAT THIS SUITE GUARDS
# ----------------------
# maechen.fayth must declare a lane so it draws from its own capacity rather than the
# task pool, must not set an assignee, and must use Opus 5 (verified 2026-09-11).
#
# maechen.md must explicitly encode all four items the bead requires. The test checks
# by grep so it fails if a future edit removes any of the required properties.
#
# conf.sh must declare SPIRA_MAECHEN_* keys with correct defaults, and the maechen
# lane must appear in SPIRA_LANES.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# -------------------------------------------------------
# Each brief property is verified by grepping for a string that would NOT appear in a
# brief that omits the property. A brief with no positive-control discussion passes all
# four of the other checks; the "positive control" grep is the one that fails.
#
# covers: spira/chamber/maechen.fayth spira/chamber/maechen.md spira/conf.sh
# hermetic-ok: no database, no systemd; reads files only
# scar: unrecorded
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in output"; esac; }
lack() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in output" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/no-such.conf"
FAYTH="$HERE/chamber/maechen.fayth"
BRIEF="$HERE/chamber/maechen.md"

run_conf() {
    env -i HOME="$T" PATH="/usr/bin:/bin" SPIRA_CONF="$NONE" \
        bash -c ". '$HERE/conf.sh' && $1" 2>/dev/null
}

echo "test-maechen.sh"

# ==========================================================================================
echo
echo "maechen.fayth — file exists"
# ==========================================================================================
if [ -f "$FAYTH" ]; then ok "maechen.fayth exists"; else bad "maechen.fayth" "not found at $FAYTH"; fi

# ==========================================================================================
echo
echo "maechen.md — file exists"
# ==========================================================================================
if [ -f "$BRIEF" ]; then ok "maechen.md exists"; else bad "maechen.md" "not found at $BRIEF"; fi

# ==========================================================================================
echo
echo "maechen.fayth — FAYTH_MODEL=claude-opus-5 (verified 2026-09-11)"
# ==========================================================================================
# The id was probed, not assumed. An id the CLI rejects does not fail loudly — every summon
# dies and the role looks merely quiet. Verify the fayth names the correct model.
model="$(run_conf ". '$FAYTH' && printf '%s' \"\${FAYTH_MODEL:-}\"")"
is "FAYTH_MODEL is claude-opus-5" "claude-opus-5" "$model"

# ==========================================================================================
echo
echo "maechen.fayth — FAYTH_LANE=maechen (draws from own capacity, not the task pool)"
# ==========================================================================================
lane="$(run_conf ". '$FAYTH' && printf '%s' \"\${FAYTH_LANE:-}\"")"
is "FAYTH_LANE=maechen" "maechen" "$lane"

# ==========================================================================================
echo
echo "maechen.fayth — no FAYTH_ASSIGNEE set (partition labels come from predicate only)"
# ==========================================================================================
assignee="$(run_conf ". '$FAYTH' && printf '%s' \"\${FAYTH_ASSIGNEE:-UNSET}\"")"
is "FAYTH_ASSIGNEE is not set" "UNSET" "$assignee"

# ==========================================================================================
echo
echo "maechen.fayth — FAYTH_EXCLUDE_LABELS excludes spira-poison"
# ==========================================================================================
excl="$(run_conf ". '$FAYTH' && printf '%s' \"\${FAYTH_EXCLUDE_LABELS:-}\"")"
want "FAYTH_EXCLUDE_LABELS contains spira-poison" "spira-poison" "$excl"

# ==========================================================================================
echo
echo "spira_fayths — maechen appears in the persona roster"
# ==========================================================================================
# POSITIVE CONTROL: enumerating chamber/*.fayth discovers maechen automatically; this fails
# if the file is removed or renamed.
export SPIRA_HOME="$HERE"
export SPIRA_CONF="$NONE"
export SPIRA_RUN="$T"
. "$HERE/lib.sh"
roster="$(spira_fayths)"
want "maechen appears in spira_fayths" "maechen" "$roster"
want "builder also appears (roster is not empty)"  "builder"  "$roster"

# ==========================================================================================
echo
echo "conf.sh — SPIRA_MAECHEN_LABEL is in the key list and defaults to maechen-sweep"
# ==========================================================================================
keys="$(run_conf 'printf "%s" "$SPIRA_CONF_KEYS"')"
want "SPIRA_MAECHEN_LABEL is in the key list"           "SPIRA_MAECHEN_LABEL"           "$keys"
want "SPIRA_MAECHEN_REMEDY_LABEL is in the key list"    "SPIRA_MAECHEN_REMEDY_LABEL"    "$keys"
want "SPIRA_MAECHEN_LANDING_INTERVAL is in the key list" "SPIRA_MAECHEN_LANDING_INTERVAL" "$keys"
want "SPIRA_MAECHEN_MAX_GAP_SECONDS is in the key list" "SPIRA_MAECHEN_MAX_GAP_SECONDS" "$keys"
want "SPIRA_MAECHEN_MAX_BEADS is in the key list"       "SPIRA_MAECHEN_MAX_BEADS"       "$keys"

lbl="$(run_conf 'printf "%s" "$SPIRA_MAECHEN_LABEL"')"
is "SPIRA_MAECHEN_LABEL defaults to maechen-sweep" "maechen-sweep" "$lbl"

remedy="$(run_conf 'printf "%s" "$SPIRA_MAECHEN_REMEDY_LABEL"')"
is "SPIRA_MAECHEN_REMEDY_LABEL defaults to maechen-remedy" "maechen-remedy" "$remedy"

max_beads="$(run_conf 'printf "%s" "$SPIRA_MAECHEN_MAX_BEADS"')"
is "SPIRA_MAECHEN_MAX_BEADS defaults to 3" "3" "$max_beads"

gap="$(run_conf 'printf "%s" "$SPIRA_MAECHEN_MAX_GAP_SECONDS"')"
is "SPIRA_MAECHEN_MAX_GAP_SECONDS defaults to 10800" "10800" "$gap"

interval="$(run_conf 'printf "%s" "$SPIRA_MAECHEN_LANDING_INTERVAL"')"
is "SPIRA_MAECHEN_LANDING_INTERVAL defaults to 25" "25" "$interval"

# ==========================================================================================
echo
echo "conf.sh — maechen lane is in SPIRA_LANES default"
# ==========================================================================================
lanes="$(run_conf 'printf "%s" "$SPIRA_LANES"')"
want "maechen is in SPIRA_LANES" "maechen" "$lanes"

# ==========================================================================================
echo
echo "maechen.md — encodes the five-step pass"
# ==========================================================================================
brief="$(cat "$BRIEF")"
want "brief contains Census step"   "Census"   "$brief"
want "brief contains Select step"   "Select"   "$brief"
want "brief contains Diagnose step" "Diagnose" "$brief"
want "brief contains Design step"   "Design"   "$brief"
want "brief contains Record step"   "Record"   "$brief"

# ==========================================================================================
echo
echo "maechen.md — encodes the three-occurrence threshold"
# ==========================================================================================
want "brief names the three-occurrence threshold" "three or more occurrences" "$brief"
want "brief explains two is a coincidence"        "coincidence"                "$brief"

# ==========================================================================================
echo
echo "maechen.md — encodes all four output-bar properties"
# ==========================================================================================
# Property 1: concrete location
want "brief names concrete location requirement" "file:line" "$brief"

# Property 2: observed evidence (command + output)
want "brief requires observed evidence (command output)" "command run and what it printed" "$brief"

# Property 3: acceptance criteria with positive control
want "brief requires acceptance criteria"  "Acceptance criteria"  "$brief"
want "brief requires a positive control"   "positive control"     "$brief"

# Property 4: failure class and occurrence count
want "brief requires failure class label"     "failure class"      "$brief"
want "brief requires occurrence count"        "occurrence count"   "$brief"

# ==========================================================================================
echo
echo "maechen.md — encodes the per-pass output bound"
# ==========================================================================================
want "brief references SPIRA_MAECHEN_MAX_BEADS" "SPIRA_MAECHEN_MAX_BEADS" "$brief"
want "brief explains flooding consequence"      "flooded"                  "$brief"

# ==========================================================================================
echo
echo "maechen.md — encodes the closing rule (quiet pass is distinguishable)"
# ==========================================================================================
want "brief requires recording the pass" "maechen.log"      "$brief"
want "brief outlaws silence"             "Silence is what"  "$brief"

# ==========================================================================================
echo
echo "maechen.md — all \$SPIRA_HOME paths resolve to existing files"
# ==========================================================================================
# POSITIVE CONTROL: plant a doubled segment (e.g. spira/spira/census.sh) and this fails.
# law-a-rename-repoints-no-reader: a persona edit that renames or moves a helper is caught
# by the same check without manual update.
_path_count=0
while IFS= read -r rel; do
    _path_count=$((_path_count+1))
    if [ -f "$HERE/$rel" ]; then
        ok "persona path resolves: \$SPIRA_HOME/$rel"
    else
        bad "persona path missing" "\$SPIRA_HOME/$rel not found under $HERE"
    fi
done < <(grep -oE '\$SPIRA_HOME/[^"'"'"' )]+' "$BRIEF" | sed 's|^\$SPIRA_HOME/||' | sort -u)
if [ "$_path_count" -gt 0 ]; then
    ok "persona names at least one \$SPIRA_HOME path (positive-control anchor)"
else
    bad "persona path extraction" "grep found no \$SPIRA_HOME paths in maechen.md — positive control is absent"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
