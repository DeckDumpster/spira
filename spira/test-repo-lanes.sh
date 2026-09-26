#!/usr/bin/env bash
#
# test-repo-lanes.sh — repo-map lanes column: parsing, mode expansion, and validation.
#
# ACCEPTANCE CRITERIA (bead sp-5q5mi, revised sp-4k8bf):
#   1. Six-column row (no lanes column) admits all lanes (no restriction declared).
#   2. Seven-column row naming a mode yields that mode's lane set.
#   3. Seven-column row listing lanes explicitly yields exactly those lanes.
#   4. Unknown mode name is a hard parse error naming the row.
#   5. Unknown lane label is a hard parse error naming the row.
#   6. Positive control: a valid row still parses after the above checks.
#   7. Empty lanes field (trailing pipe, nothing after it) admits all lanes.
#
# POSITIVE CONTROLS. Each absence assertion is preceded by a presence assertion on the same
# path, so a check pointed at the wrong thing and a check that found nothing look different
# (law-absence-needs-a-positive-control).
#
# tier: T1
# covers: spira/lib.sh spira/repo-map.example spira/doctor.sh
# defect: sp-5q5mi
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"


T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# A MINIMAL ENVIRONMENT with non-default label values where possible, so assertions against
# defaults are not trivially satisfied by literals in the code
# (law-gates-run-in-a-clean-environment).
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T"
export SPIRA_DB="$T/no-db"
# Non-default label values to prove mode expansion reads conf vars, not literals.
export SPIRA_PLAN_LABEL="plan"
export SPIRA_INCIDENT_LABEL="incident"
export SPIRA_GROOMER_LABEL="groom"
export SPIRA_MAECHEN_LABEL="maechen-sweep"
export SPIRA_SPIKE_LABEL="spike"
export SPIRA_CZAR_LABEL="czar-trigger"

# A throwaway repo-map. SPIRA_REPO_MAP is set per section.
MAP="$T/repo-map"

. "$HERE/lib.sh"

# ==========================================================================================
echo
echo "criterion 1 — six-column row (no lanes column) admits all lanes"
# ==========================================================================================
printf 'alpha | /tmp/alpha | push | origin/main | | true\n' > "$MAP"
export SPIRA_REPO_MAP="$MAP"

# POSITIVE CONTROL: the row IS found (repo_field returns something for it).
is "six-col: path column resolves" "/tmp/alpha" "$(repo_field alpha path)"

out="$(spira_repo_lanes alpha)"
want "six-col: plan in result"              "plan"          "$out"
want "six-col: incident in result"          "incident"      "$out"
want "six-col: groom in result"             "groom"         "$out"
want "six-col: maechen-sweep in result"     "maechen-sweep" "$out"
want "six-col: spike in result"             "spike"         "$out"
want "six-col: czar-trigger in result"      "czar-trigger"  "$out"

# ==========================================================================================
echo
echo "criterion 2 — mode names expand to their lane sets"
# ==========================================================================================
cat > "$MAP" <<'ROW'
alpha | /tmp/alpha | push | origin/main | | true | consume
beta  | /tmp/beta  | push | origin/main | | true | develop
gamma | /tmp/gamma | push | origin/main | | true | self
ROW

# consume -> plan only
out_c="$(spira_repo_lanes alpha)"
is   "consume: yields plan"                 "plan"         "$out_c"
nowant "consume: no incident"               "incident"     "$out_c"
nowant "consume: no groom"                  "groom"        "$out_c"

# develop -> plan incident groom spike
out_d="$(spira_repo_lanes beta)"
want   "develop: contains plan"             "plan"         "$out_d"
want   "develop: contains incident"         "incident"     "$out_d"
want   "develop: contains groom"            "groom"        "$out_d"
want   "develop: contains spike"            "spike"        "$out_d"
nowant "develop: no maechen-sweep"          "maechen-sweep" "$out_d"
nowant "develop: no czar-trigger"           "czar-trigger" "$out_d"

# self -> all six labels
out_s="$(spira_repo_lanes gamma)"
want   "self: contains plan"                "plan"         "$out_s"
want   "self: contains incident"            "incident"     "$out_s"
want   "self: contains groom"              "groom"        "$out_s"
want   "self: contains spike"              "spike"        "$out_s"
want   "self: contains maechen-sweep"       "maechen-sweep" "$out_s"
want   "self: contains czar-trigger"        "czar-trigger" "$out_s"

# gate column is unchanged when lanes is present (positive control that gate still parses).
is "gate still reads correctly with lanes present" "true" "$(repo_field alpha gate)"
is "gate still reads correctly for self row"       "true" "$(repo_field gamma gate)"

# ==========================================================================================
echo
echo "criterion 3 — explicit lane list yields exactly those lanes"
# ==========================================================================================
printf 'alpha | /tmp/alpha | push | origin/main | | true | plan,incident\n' > "$MAP"

out_e="$(spira_repo_lanes alpha)"
want   "explicit: contains plan"            "plan"         "$out_e"
want   "explicit: contains incident"        "incident"     "$out_e"
nowant "explicit: no groom"                 "groom"        "$out_e"
nowant "explicit: no spike"                 "spike"        "$out_e"

# Single lane
printf 'alpha | /tmp/alpha | push | origin/main | | true | spike\n' > "$MAP"
out_sp="$(spira_repo_lanes alpha)"
is     "single lane spike: exact"           "spike"        "$out_sp"
nowant "single lane spike: no plan"         "plan"         "$out_sp"

# ==========================================================================================
echo
echo "criterion 4 — unknown mode name is a hard parse error"
# ==========================================================================================
# POSITIVE CONTROL FIRST: a valid mode name succeeds.
printf 'alpha | /tmp/alpha | push | origin/main | | true | develop\n' > "$MAP"
is "positive: valid mode develop succeeds" "0" "$(spira_repo_lanes alpha >/dev/null 2>&1; echo $?)"

# Unknown mode name: exits non-zero AND names the row in stderr.
printf 'alpha | /tmp/alpha | push | origin/main | | true | fullaccess\n' > "$MAP"
rc=0; err=""; err="$(spira_repo_lanes alpha 2>&1)" || rc=$?
is     "unknown mode: exits non-zero"       "1" "$rc"
want   "unknown mode: names the row"        "alpha"         "$err"

# ==========================================================================================
echo
echo "criterion 5 — unknown lane label is a hard parse error"
# ==========================================================================================
# POSITIVE CONTROL: valid lane list succeeds.
printf 'alpha | /tmp/alpha | push | origin/main | | true | plan,groom\n' > "$MAP"
is "positive: valid lane list succeeds" "0" "$(spira_repo_lanes alpha >/dev/null 2>&1; echo $?)"

# Unknown lane label: exits non-zero AND names the row in stderr.
printf 'alpha | /tmp/alpha | push | origin/main | | true | plan,bogus-lane\n' > "$MAP"
rc=0; err=""; err="$(spira_repo_lanes alpha 2>&1)" || rc=$?
is     "unknown lane: exits non-zero"       "1" "$rc"
want   "unknown lane: names the row"        "alpha"         "$err"
want   "unknown lane: mentions the bad label" "bogus-lane"  "$err"

# ==========================================================================================
echo
echo "criterion 7 — empty lanes field admits all lanes"
# ==========================================================================================
# A trailing pipe with nothing after it: `name | path | land | base | format | gate | `
printf 'alpha | /tmp/alpha | push | origin/main | | true | \n' > "$MAP"
out_empty="$(spira_repo_lanes alpha)"
want "empty lanes field: plan in result"          "plan"          "$out_empty"
want "empty lanes field: incident in result"      "incident"      "$out_empty"
want "empty lanes field: groom in result"         "groom"         "$out_empty"
want "empty lanes field: maechen-sweep in result" "maechen-sweep" "$out_empty"
want "empty lanes field: spike in result"         "spike"         "$out_empty"
want "empty lanes field: czar-trigger in result"  "czar-trigger"  "$out_empty"

# ==========================================================================================
echo
echo "gate column unaffected — pipe-containing gates still parse correctly"
# ==========================================================================================
# A gate with || and case-statement pipes, plus a lanes column.
cat > "$MAP" <<'ROW'
alpha | /tmp/alpha | queue | origin/main | | bash a.sh || bash b.sh | develop
ROW
gate_out="$(repo_field alpha gate)"
want   "pipe gate: contains first command"  "bash a.sh"    "$gate_out"
want   "pipe gate: contains second command" "bash b.sh"    "$gate_out"
nowant "pipe gate: lanes not in gate"       "develop"      "$gate_out"
is     "pipe gate: lanes still parsed"      "plan incident groom spike" "$(spira_repo_lanes alpha)"

# ==========================================================================================
echo
echo "backward compat — six-column rows with complex gates parse without lane artifact"
# ==========================================================================================
cat > "$MAP" <<'ROW'
alpha | /tmp/alpha | queue | origin/main | | bash a.sh || bash b.sh
ROW
bc_gate="$(repo_field alpha gate)"
bc_lanes="$(spira_repo_lanes alpha)"
want   "six-col complex gate: first cmd"    "bash a.sh"    "$bc_gate"
want   "six-col complex gate: second cmd"   "bash b.sh"    "$bc_gate"
want   "six-col: plan in lanes"             "plan"         "$bc_lanes"
want   "six-col: incident in lanes"         "incident"     "$bc_lanes"
nowant "six-col: gate fragment not in lanes" "bash"        "$bc_lanes"

echo
tl_summary
