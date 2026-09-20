#!/usr/bin/env bash
#
# test-effective-lanes.sh — .spira/modes intersection with repo-map and SPIRA_FAYTHS.
#
# ACCEPTANCE CRITERIA (bead sp-czsf4):
#   1. modes=self, map=develop  -> maechen-sweep refused, diagnostic names repo-map.
#   2. modes=consume, map=self  -> plan alone; refused lanes named .spira/modes.
#   3. SPIRA_FAYTHS missing a fayth -> that lane absent; diagnostic names SPIRA_FAYTHS.
#   4. No .spira/modes, no lanes column -> plan alone, no refusal diagnostics.
#   5. Missing working copy -> no crash; .spira/modes treated as absent.
#   6. .spira/modes parse error -> modes-error line; effective falls back to plan.
#
# POSITIVE CONTROLS. Each absence assertion is preceded by a presence assertion so
# a check pointed at the wrong thing and a check that found nothing look different
# (law-absence-needs-a-positive-control).
#
# covers: spira/lib.sh spira/doctor.sh
# defect: sp-czsf4
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run" "$T/chamber"

# Non-default label values where possible so assertions are not trivially satisfied
# by literals in the code (law-gates-run-in-a-clean-environment).
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T"
export SPIRA_DB="$T/no-db"
export SPIRA_PLAN_LABEL="plan"
export SPIRA_INCIDENT_LABEL="incident"
export SPIRA_GROOMER_LABEL="groom"
export SPIRA_MAECHEN_LABEL="maechen-sweep"
export SPIRA_SPIKE_LABEL="spike"
export SPIRA_CZAR_LABEL="czar-trigger"

MAP="$T/repo-map"
export SPIRA_REPO_MAP="$MAP"

# Minimal chamber — each fayth exposes only the one label it owns.
for _f_name in builder ops groomer maechen spike czar; do
    case "$_f_name" in
        builder) _f_label='${SPIRA_PLAN_LABEL:-plan}' ;;
        ops)     _f_label='${SPIRA_INCIDENT_LABEL:-incident}' ;;
        groomer) _f_label='${SPIRA_GROOMER_LABEL:-groom}' ;;
        maechen) _f_label='${SPIRA_MAECHEN_LABEL:-maechen-sweep}' ;;
        spike)   _f_label='${SPIRA_SPIKE_LABEL:-spike}' ;;
        czar)    _f_label='${SPIRA_CZAR_LABEL:-czar-trigger}' ;;
    esac
    printf 'FAYTH_LABELS="%s"\n' "$_f_label" > "$T/chamber/$_f_name.fayth"
done
unset _f_name _f_label

. "$HERE/lib.sh"

REPO="$T/repo"
mkdir -p "$REPO"

# ==========================================================================================
echo
echo "criterion 1 — modes=self, map=develop: maechen-sweep refused by repo-map"
# ==========================================================================================
printf 'alpha | %s | push | origin/main | | true | develop\n' "$REPO" > "$MAP"
printf 'self\n' > "$REPO/.spira/modes"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: a lane that IS in both is present in effective.
out1="$(_spira_lane_diag alpha "$REPO")"
want "crit1 pos: plan in effective"        "effective:" "$out1"
want "crit1 pos: plan present"             "plan"       "$out1"
want "crit1 pos: incident present"         "plan incident" "$out1"

# maechen-sweep: in modes (self) but NOT in map (develop) -> refused by repo-map
want  "crit1: maechen-sweep refused"       "refused: maechen-sweep"   "$out1"
want  "crit1: refuser is repo-map"         "refused: maechen-sweep by repo-map" "$out1"
# czar-trigger also not in develop
want  "crit1: czar-trigger refused"        "refused: czar-trigger by repo-map" "$out1"
# plan, incident, groom, spike are in effective (develop includes them)
nowant "crit1: plan not refused"           "refused: plan"            "$out1"
nowant "crit1: incident not refused"       "refused: incident"        "$out1"
nowant "crit1: groom not refused"          "refused: groom"           "$out1"
nowant "crit1: spike not refused"          "refused: spike"           "$out1"
# fayths and modes do NOT appear as refusers for maechen-sweep
nowant "crit1: SPIRA_FAYTHS not named"    "maechen-sweep by SPIRA_FAYTHS" "$out1"
nowant "crit1: modes not named"           "maechen-sweep by .spira/modes" "$out1"

rm -f "$REPO/.spira/modes"

# ==========================================================================================
echo
echo "criterion 2 — modes=consume, map=self: plan alone; refused lanes name .spira/modes"
# ==========================================================================================
printf 'alpha | %s | push | origin/main | | true | self\n' "$REPO" > "$MAP"
printf 'consume\n' > "$REPO/.spira/modes"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: plan is in effective.
out2="$(_spira_lane_diag alpha "$REPO")"
want "crit2 pos: effective contains plan"  "effective: plan"          "$out2"

# All non-plan lanes are refused by .spira/modes
want  "crit2: incident refused by modes"  "refused: incident by .spira/modes"     "$out2"
want  "crit2: groom refused by modes"     "refused: groom by .spira/modes"        "$out2"
want  "crit2: maechen refused by modes"   "refused: maechen-sweep by .spira/modes" "$out2"
want  "crit2: spike refused by modes"     "refused: spike by .spira/modes"        "$out2"
want  "crit2: czar refused by modes"      "refused: czar-trigger by .spira/modes" "$out2"
# repo-map and SPIRA_FAYTHS do NOT appear as refusers (they grant all)
nowant "crit2: repo-map not named"        "by repo-map"              "$out2"
nowant "crit2: SPIRA_FAYTHS not named"   "by SPIRA_FAYTHS"          "$out2"

rm -f "$REPO/.spira/modes"

# ==========================================================================================
echo
echo "criterion 3 — SPIRA_FAYTHS missing maechen: maechen-sweep refused by SPIRA_FAYTHS"
# ==========================================================================================
printf 'alpha | %s | push | origin/main | | true | self\n' "$REPO" > "$MAP"
printf 'self\n' > "$REPO/.spira/modes"
# Roster excludes maechen — no maechen.fayth needed, SPIRA_FAYTHS is what matters.
export SPIRA_FAYTHS="builder ops groomer spike czar"

# POSITIVE CONTROL: a lane whose fayth IS configured is in effective.
out3="$(_spira_lane_diag alpha "$REPO")"
want "crit3 pos: plan in effective"       "plan"                      "$out3"
want "crit3 pos: incident in effective"   "incident"                  "$out3"
nowant "crit3 pos: maechen NOT effective" "effective:.*maechen-sweep" "$out3"

# maechen-sweep: in both map and modes but fayths doesn't include it
want  "crit3: maechen refused"            "refused: maechen-sweep"    "$out3"
want  "crit3: refuser is SPIRA_FAYTHS"   "refused: maechen-sweep by SPIRA_FAYTHS" "$out3"
nowant "crit3: repo-map not named"        "maechen-sweep by repo-map" "$out3"
nowant "crit3: modes not named"           "maechen-sweep by .spira/modes" "$out3"

rm -f "$REPO/.spira/modes"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# ==========================================================================================
echo
echo "criterion 4 — no .spira/modes, no lanes column: plan alone, no refusal lines"
# ==========================================================================================
printf 'alpha | %s | push | origin/main | | true\n' "$REPO" > "$MAP"
# No .spira/modes file.
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: plan is present in effective (proves the check runs).
out4="$(_spira_lane_diag alpha "$REPO")"
want "crit4 pos: effective line present"  "effective:"                "$out4"
want "crit4 pos: plan in effective"       "plan"                      "$out4"

# No refused lines at all.
nowant "crit4: no refused lines"          "refused:"                  "$out4"

# ==========================================================================================
echo
echo "criterion 5 — missing working copy: no crash, .spira/modes treated as absent"
# ==========================================================================================
printf 'alpha | /nonexistent/path | push | origin/main | | true | self\n' > "$MAP"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

rc5=0; out5="$(_spira_lane_diag alpha "/nonexistent/path")" || rc5=$?
is    "crit5: exits cleanly"              "0"       "$rc5"
want  "crit5: effective line present"     "effective:" "$out5"
# With no .spira/modes the effective set is map_lanes ∩ fayths_lanes = all-of-self
want  "crit5: plan in effective"          "plan"      "$out5"
# No modes file -> modes is not a refuser for anything map+fayths both grant
nowant "crit5: modes not a refuser"       "by .spira/modes" "$out5"

# ==========================================================================================
echo
echo "criterion 6 — .spira/modes parse error: modes-error line, falls back to plan"
# ==========================================================================================
printf 'alpha | %s | push | origin/main | | true | self\n' "$REPO" > "$MAP"
printf 'badmode\n' > "$REPO/.spira/modes"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: a valid modes file produces no modes-error line.
printf 'self\n' > "$REPO/.spira/modes"
out6_ok="$(_spira_lane_diag alpha "$REPO")"
nowant "crit6 pos: valid modes: no error" "modes-error:" "$out6_ok"

# Invalid modes file produces modes-error line.
printf 'badmode\n' > "$REPO/.spira/modes"
out6="$(_spira_lane_diag alpha "$REPO")"
want  "crit6: modes-error line present"   "modes-error:"   "$out6"
want  "crit6: effective line still present" "effective:"   "$out6"

rm -f "$REPO/.spira/modes"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
