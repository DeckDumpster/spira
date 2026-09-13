#!/usr/bin/env bash
#
# test-cockpit-unclaimable.sh — SP_NEXT shows the persona that WILL claim a bead,
#                               not the one the partition query found it under.
#
# THE DEFECT. sp-f8vry: a bead carrying fayth:ops on spira,plan labels appears in
# builder's partition query (because builder's labels are a subset of its labels). Before
# this fix the aggregator stamped it as "_partition=builder" and emitted
# "SP_NEXT0=P1 builder sp-foo ..." — telling the operator builder would claim it.
# Builder cannot claim it (fayth:ops excludes builder); ops cannot claim it (no incident
# label). The bead is unclaimable, but the panel said "builder" for fifteen hours.
#
# THE FIX. The aggregator now receives the partition map (label-set → persona name) and
# checks each bead's fayth: preference at render time. If a preference is present and the
# named persona's labels are not all on the bead, the partition label becomes "unclaimable"
# rather than the persona that won the label query but cannot actually claim the work.
#
# defect: sp-f8vry
# covers: spira/cockpit.sh
# hermetic-ok: mock bd binary, no systemd or database
# scar: a bead carrying fayth:ops on spira,plan labels appeared in builder's partition query; the panel said "builder" for fifteen hours while the bead was unclaimable by any persona.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

# run_core <bd-binary> -> stdout of cockpit.sh core (SP_NEXT* and SP_READY keys)
# SPIRA_SCOPE_LABEL is not set, so conf.sh will default it to "spira".
# This means partition labels are "spira,plan" and "spira,incident" — not bare "plan".
# The bead fixture labels must include "spira" to match the partition queries.
run_core() {
    local bd_path="$1"
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test "SPIRA_FAYTHS=builder ops" \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_BD="$bd_path" \
        bash "$HERE/cockpit.sh" core 2>/dev/null
}

# Build a mock bd that returns a given JSON array when queried for the plan partition
# (ready ... --label spira,plan), and [] for everything else.
# The LAST --label argument in the partition query is the partition-specific one
# (e.g. "spira,plan" for builder, "spira,incident" for ops) — scope comes first.
make_bd() {     # make_bd <path> <label-to-match> <json-to-return>
    local path="$1" match="$2" payload="$3"
    cat > "$path" <<EOF
#!/usr/bin/env bash
is_ready=0; last_label=""; prev_arg=""
for arg in "\$@"; do
    [ "\$arg" = "ready" ] && is_ready=1
    [ "\$prev_arg" = "--label" ] && last_label="\$arg"
    prev_arg="\$arg"
done
if [ "\$is_ready" = 1 ] && [ "\$last_label" = "$match" ]; then
    printf '%s\n' '$payload'
else
    printf '[]\n'
fi
EOF
    chmod +x "$path"
}

# ========================================================================================
echo "case 1 — positive control: a claimable builder bead shows 'builder', not 'unclaimable'"
# ========================================================================================
# A bead with {plan, spira} and no fayth: preference appears in builder's partition query
# and has no narrowing preference. The cockpit must show "builder", not "unclaimable".
# Without this case, an implementation that always shows "unclaimable" would pass case 2.
make_bd "$TMP/bd-1" "spira,plan" \
    '[{"id":"sp-uc1a","title":"claimable builder bead","status":"open","issue_type":"task","priority":1,"labels":["plan","repo:spira","spira"]}]'

out="$(run_core "$TMP/bd-1")"
want   "claimable bead appears in NEXT"               "sp-uc1a"       "$out"
want   "claimable bead shows builder"                  "builder"       "$out"
nowant "claimable bead does NOT show unclaimable"      "unclaimable"   "$out"

# ========================================================================================
echo
echo "case 2 — fayth:ops on spira,plan labels shows 'unclaimable', not 'builder'"
# ========================================================================================
# The fifteen-hour strand. A bead carrying fayth:ops AND the builder partition labels
# appears in builder's partition query because builder's labels are a subset.
# The cockpit used to stamp it as "builder". This case asserts that the cockpit now shows
# "unclaimable" instead: builder is excluded by the fayth:ops preference, and ops is
# excluded by its own partition check (the bead has no incident label).
make_bd "$TMP/bd-2" "spira,plan" \
    '[{"id":"sp-uc2a","title":"fayth:ops on plan labels","status":"open","issue_type":"task","priority":1,"labels":["fayth:ops","plan","repo:spira","spira"]}]'

out="$(run_core "$TMP/bd-2")"
want   "unclaimable bead appears in NEXT"             "sp-uc2a"       "$out"
want   "unclaimable bead shows unclaimable"           "unclaimable"   "$out"
nowant "unclaimable bead does NOT show builder"       "builder"       "$out"

# ========================================================================================
echo
echo "case 3 — fayth:ops on incident labels shows 'ops' (the preference matches)"
# ========================================================================================
# A bead with fayth:ops AND ops's partition labels (incident, spira) is correctly claimable
# by ops. The cockpit must show "ops", not "unclaimable". The preference matches the
# persona whose labels are all present on the bead.
make_bd "$TMP/bd-3" "spira,incident" \
    '[{"id":"sp-uc3a","title":"fayth:ops on incident labels","status":"open","issue_type":"task","priority":1,"labels":["fayth:ops","incident","repo:spira","spira"]}]'

out="$(run_core "$TMP/bd-3")"
want   "ops-fayth ops-partition bead appears in NEXT"      "sp-uc3a"      "$out"
want   "ops-fayth ops-partition shows ops"                 "ops"          "$out"
nowant "ops-fayth ops-partition does NOT show unclaimable" "unclaimable"  "$out"

# ========================================================================================
echo
echo "case 4 — mixed: claimable and unclaimable beads in the same partition query"
# ========================================================================================
# Both beads appear in builder's query (both have spira,plan labels). One has fayth:ops
# and is unclaimable; the other has no preference and is claimable. Both must render
# correctly in the same SP_NEXT output: the claimable one shows "builder", the
# unclaimable one shows "unclaimable".
make_bd "$TMP/bd-4" "spira,plan" \
    '[{"id":"sp-uc4a","title":"claimable: no pref","status":"open","issue_type":"task","priority":1,"labels":["plan","repo:spira","spira"]},{"id":"sp-uc4b","title":"unclaimable: fayth:ops","status":"open","issue_type":"task","priority":1,"labels":["fayth:ops","plan","repo:spira","spira"]}]'

out="$(run_core "$TMP/bd-4")"
want   "mixed: claimable bead in NEXT"              "sp-uc4a"       "$out"
want   "mixed: unclaimable bead in NEXT"            "sp-uc4b"       "$out"
want   "mixed: claimable shows builder"             "builder"       "$out"
want   "mixed: unclaimable shows unclaimable"       "unclaimable"   "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
