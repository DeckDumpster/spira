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
# tier: T1
# covers: cockpit-collect/src/* spira/unclaimable.py UC-dispatch-17
# hermetic-ok: mock bd binary, no systemd or database
# scar: a bead carrying fayth:ops on spira,plan labels appeared in builder's partition query; the panel said "builder" for fifteen hours while the bead was unclaimable by any persona.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# line_for_id <id> <sp_next-output> -> the one line naming <id>, or empty if absent.
# Case 4 needs to know WHICH line a given id landed on, not merely whether a label
# string appears anywhere in the whole block (that could not tell sp-uc4a's line
# apart from sp-uc4b's).
line_for_id() { printf '%s\n' "$2" | grep -F "$1" || true; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"
: "${SPIRA_SCOPE_LABEL:=$(basename "$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null || printf '')")}"

# THE READY SET IS THE MACHINE'S (sp-7g5q6; sp-v62vn: the only mode). The NEXT rows come
# from spira-claim's `ready-count <partition labels> <exclude> --json`, which takes the
# lifecycle machine's READY rows and reads bd only for their content. The stand-in below
# (testlib lc_mirror_bd) answers spira-lc `list` from the mock bd's store — every open
# fixture bead is a READY row — and spira-claim's own label predicate decides which
# partition query each bead lands in, as it does in production.
LC="$TMP/lc"
lc_mirror_bd "$LC"

# run_core <bd-binary> -> stdout of cockpit-collect probe core (SP_NEXT* and SP_READY keys)
# SPIRA_SCOPE_LABEL is passed explicitly so the cockpit's partition queries use the
# same value as the bead fixture labels below.
run_core() {
    local bd_path="$1"
    # SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP/SPIRA_FAYTHS/SPIRA_SCOPE_LABEL/SPIRA_ASK_LABEL/
    # SPIRA_CI_LABEL/SPIRA_BD are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
    # CONFIG): declare via tl_config and thread SPIRA_TOML through env -i, which clears it.
    # round 3 fix (pattern 6): SPIRA_CHAMBER no longer derives from SPIRA_HOME — without
    # it, cockpit-collect cannot find chamber/builder.fayth or chamber/ops.fayth to learn
    # either persona's FAYTH_LABELS, so it can never say who would claim anything.
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_FAYTHS="builder ops" SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci SPIRA_BD="$bd_path" \
        SPIRA_CHAMBER="$HERE/chamber"
    env -i PATH="$LC:$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_TOML="$SPIRA_TOML" \
        cockpit-collect probe core 2>/dev/null
}

# make_bd <path> <json> — a mock bd whose store is <json>: the lifecycle stand-in's read
# (`list --all`) and spira-claim's content read (`list --id ...`) get it, every other query
# gets [] — so no other cockpit section sees these beads.
make_bd() {
    local path="$1" payload="$2"
    cat > "$path" <<EOF
#!/usr/bin/env bash
case " \$* " in
    *" list --all "*|*" list --id "*) printf '%s\n' '$payload' ;;
    *) printf '[]\n' ;;
esac
EOF
    chmod +x "$path"
}

# ========================================================================================
echo "case 1 — positive control: a claimable builder bead shows 'builder', not 'unclaimable'"
# ========================================================================================
# A bead with {plan, scope} and no fayth: preference appears in builder's partition query
# and has no narrowing preference. The cockpit must show "builder", not "unclaimable".
# Without this case, an implementation that always shows "unclaimable" would pass case 2.
make_bd "$TMP/bd-1" \
    '[{"id":"sp-uc1a","title":"claimable builder bead","status":"open","issue_type":"task","priority":1,"labels":["plan","repo:spira","'"${SPIRA_SCOPE_LABEL}"'"]}]'

out="$(run_core "$TMP/bd-1")"
want   "claimable bead appears in NEXT"               "sp-uc1a"       "$out"
want   "claimable bead shows builder"                  "builder"       "$out"
nowant "claimable bead does NOT show unclaimable"      "unclaimable"   "$out"

# ========================================================================================
echo
echo "case 2 — fayth:ops on plan labels shows 'unclaimable', not 'builder'"
# ========================================================================================
# The fifteen-hour strand. A bead carrying fayth:ops AND the builder partition labels
# appears in builder's partition query because builder's labels are a subset.
# The cockpit used to stamp it as "builder". This case asserts that the cockpit now shows
# "unclaimable" instead: builder is excluded by the fayth:ops preference, and ops is
# excluded by its own partition check (the bead has no incident label).
make_bd "$TMP/bd-2" \
    '[{"id":"sp-uc2a","title":"fayth:ops on plan labels","status":"open","issue_type":"task","priority":1,"labels":["fayth:ops","plan","repo:spira","'"${SPIRA_SCOPE_LABEL}"'"]}]'

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
make_bd "$TMP/bd-3" \
    '[{"id":"sp-uc3a","title":"fayth:ops on incident labels","status":"open","issue_type":"task","priority":1,"labels":["fayth:ops","incident","repo:spira","'"${SPIRA_SCOPE_LABEL}"'"]}]'

out="$(run_core "$TMP/bd-3")"
want   "ops-fayth ops-partition bead appears in NEXT"      "sp-uc3a"      "$out"
want   "ops-fayth ops-partition shows ops"                 "ops"          "$out"
nowant "ops-fayth ops-partition does NOT show unclaimable" "unclaimable"  "$out"

# ========================================================================================
echo
echo "case 4 — mixed: claimable and unclaimable beads in the same partition query"
# ========================================================================================
# Both beads appear in builder's query (both have spira,plan labels). One has fayth:ops
# and is unclaimable; the other has no preference and is claimable. Asserting that both
# label strings appear SOMEWHERE in the output cannot tell which id got which label — an
# implementation that swapped them would still pass. Each id's OWN line is checked instead.
CASE4_JSON='[{"id":"sp-uc4a","title":"claimable: no pref","status":"open","issue_type":"task","priority":1,"labels":["plan","repo:spira","'"${SPIRA_SCOPE_LABEL}"'"]},{"id":"sp-uc4b","title":"unclaimable: fayth:ops","status":"open","issue_type":"task","priority":1,"labels":["fayth:ops","plan","repo:spira","'"${SPIRA_SCOPE_LABEL}"'"]}]'
make_bd "$TMP/bd-4" "$CASE4_JSON"

out="$(run_core "$TMP/bd-4")"
line_a="$(line_for_id sp-uc4a "$out")"
line_b="$(line_for_id sp-uc4b "$out")"
want   "mixed: sp-uc4a's own line shows builder"              "builder"      "$line_a"
nowant "mixed: sp-uc4a's own line does NOT show unclaimable"  "unclaimable"  "$line_a"
want   "mixed: sp-uc4b's own line shows unclaimable"          "unclaimable"  "$line_b"
nowant "mixed: sp-uc4b's own line does NOT show builder"      "builder"      "$line_b"

# UC-dispatch-17: the cockpit's per-id attribution must agree with the UC-16 classifier
# (spira/unclaimable.py) run over the same two beads, not merely with itself.
UC16_PARTS="builder|${SPIRA_SCOPE_LABEL},plan|
ops|${SPIRA_SCOPE_LABEL},incident|
"
# round 3 fix: unclaimable.py is explicitly a "direct invocation without conf.sh" tool
# (its own header comment) — it reads SPIRA_SCOPE_LABEL/SPIRA_CI_LABEL/SPIRA_ASK_LABEL
# from os.environ directly, by design, never through spira-config/SPIRA_TOML. tl_config
# here would be silently ignored by this one reader; keep the plain env prefix.
uc16_out="$(PARTS="$UC16_PARTS" ALL_PARTS="$UC16_PARTS" \
    SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" SPIRA_CI_LABEL=awaiting-ci SPIRA_ASK_LABEL=needs-ryan \
    unclaimable.py <<< "$CASE4_JSON")"
nowant "UC-16 classifier agrees sp-uc4a is claimable"    "sp-uc4a"               "$uc16_out"
want   "UC-16 classifier agrees sp-uc4b is unclaimable"  "UNCLAIMABLE sp-uc4b"   "$uc16_out"

tl_summary
