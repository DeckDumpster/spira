#!/usr/bin/env bash
#
# test-bead-dep-add.sh — bead.sh dep add: the harness dependency path refuses a blocks edge
# onto an incident-labelled bead, naming `bd dep relate` as the alternative, while a plain
# blocks edge between two work beads still reaches bd and is wired for real (sp-3bc6t,
# sp-ivlj4 — sp-pyowh sat blocked 22 hours on sp-kogm, the recurring alarm, this exact way).
#
# tier: T2
# covers: spira/bead.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"

echo "test-bead-dep-add.sh"

testdb_require test-bead-dep-add
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_dep_add || { printf 'test-bead-dep-add: could not build fixture database\n' >&2; exit 1; }

testdb_seed <<'JSONL'
{"id":"sp-dep-alarm","title":"recurring alarm","status":"open","issue_type":"task","labels":["incident-test","spira"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-dep-work-a","title":"ordinary work a","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-dep-epic","title":"an epic","status":"open","issue_type":"epic","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-dep-work-b","title":"ordinary work b","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

run_dep_add() {           # run_dep_add <args...> -> sets DA_OUT and DA_RC from ONE call
    # SPIRA_INCIDENT_LABEL is the registered key the refusal actually checks (the fixture
    # bead is literally labelled "incident-test"); SPIRA_ALARM_LABEL is not a registered
    # name and was never read by anything. SPIRA_RUN too — undeclared, it resolved to the
    # complete fixture's /fixture/userhome/.../run, which this sandbox cannot mkdir at all
    # (sfail round 3, pattern 7).
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_INCIDENT_LABEL="incident-test" SPIRA_RUN="$TMP/run"
    DA_OUT="$(SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
        bead.sh dep add "$@" 2>&1)"
    DA_RC=$?
}

blocks_of() {              # blocks_of <id> -> depends_on_id list, one per line
    timeout 5 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" dep list "$1" --type blocks --json 2>/dev/null \
        | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except ValueError:
    data = []
for d in data:
    print(d.get("depends_on_id") or d.get("id") or "")
' 2>/dev/null
}

# ===========================================================================================
echo
echo "T1: refuses a blocks edge onto an incident-labelled bead"
# ===========================================================================================
run_dep_add sp-dep-work-a sp-dep-alarm
wantrc "dep add onto an alarm exits non-zero" "1" "$DA_RC"
want   "the refusal names the incident label" "incident-test" "$DA_OUT"
want   "the refusal names dep relate as the alternative" "bd dep relate sp-dep-work-a sp-dep-alarm" "$DA_OUT"
is     "no edge was actually wired" "" "$(blocks_of sp-dep-work-a)"

run_dep_add sp-dep-work-a --blocked-by sp-dep-alarm
wantrc "the --blocked-by alias is refused the same way" "1" "$DA_RC"
is     "no edge was wired via the alias either" "" "$(blocks_of sp-dep-work-a)"

run_dep_add sp-dep-work-a sp-dep-alarm --type relates-to
wantrc "an explicit --type relates-to is never refused" "0" "$DA_RC"

# ===========================================================================================
echo
echo "T2: a blocks edge between two work beads passes through and is wired for real"
# ===========================================================================================
run_dep_add sp-dep-work-b sp-dep-work-a
wantrc "dep add between two work beads exits 0" "0" "$DA_RC"
is     "the blocks edge was wired in the store" "sp-dep-work-a" "$(blocks_of sp-dep-work-b)"

run_dep_remove() {        # run_dep_remove <args...> -> sets DA_OUT and DA_RC from ONE call
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_INCIDENT_LABEL="incident-test" SPIRA_RUN="$TMP/run"
    DA_OUT="$(SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
        bead.sh dep remove "$@" 2>&1)"
    DA_RC=$?
}

# ===========================================================================================
echo
echo "T3: dep remove drops an existing edge, refusing unknown ids and a missing edge"
# ===========================================================================================
run_dep_remove sp-dep-work-b sp-dep-nope
wantrc "an unknown depends-on id is refused" "1" "$DA_RC"
want   "the refusal names the unknown id" "sp-dep-nope" "$DA_OUT"
run_dep_remove sp-dep-nope sp-dep-work-a
wantrc "an unknown id is refused" "1" "$DA_RC"
is     "the existing edge survived the refusals" "sp-dep-work-a" "$(blocks_of sp-dep-work-b)"

run_dep_remove sp-dep-work-a sp-dep-work-b
wantrc "removing an edge that does not exist is refused" "1" "$DA_RC"

run_dep_remove sp-dep-work-b sp-dep-work-a
wantrc "removing the real edge exits 0" "0" "$DA_RC"
want   "it prints the removed edge" "sp-dep-work-b -> sp-dep-work-a" "$DA_OUT"
is     "the edge is gone from the store" "" "$(blocks_of sp-dep-work-b)"

# ===========================================================================================
echo
echo "T4: a blocks edge onto an epic is refused at filing; an existing one is converted"
# ===========================================================================================
types_of() {               # types_of <id> -> "<depends_on>:<type>" per edge
    timeout 5 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" dep list "$1" --json 2>/dev/null \
        | python3 -c '
import json, sys
for d in json.load(sys.stdin):
    print((d.get("depends_on_id") or d.get("id") or "") + ":" + (d.get("type") or d.get("dependency_type") or ""))
' 2>/dev/null
}

run_dep_add sp-dep-work-a sp-dep-epic
wantrc "a blocks edge onto an epic exits non-zero" "1" "$DA_RC"
want   "the refusal names parent-child as the fix" "--type parent-child" "$DA_OUT"
is     "no edge was wired onto the epic" "" "$(blocks_of sp-dep-work-a)"
run_dep_add sp-dep-work-a --blocked-by sp-dep-epic
wantrc "the --blocked-by alias is refused the same way" "1" "$DA_RC"

run_dep_add sp-dep-work-a sp-dep-epic --type parent-child
wantrc "a parent-child edge onto the epic is accepted" "0" "$DA_RC"
run_dep_remove sp-dep-work-a sp-dep-epic

timeout 10 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" dep add sp-dep-work-b sp-dep-epic --type blocks >/dev/null 2>&1
is "the planted pre-existing edge is present" "sp-dep-epic" "$(blocks_of sp-dep-work-b)"

run_dep_cmd() {            # run_dep_cmd <args...> -> sets DA_OUT and DA_RC from ONE call
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
        SPIRA_INCIDENT_LABEL="incident-test" SPIRA_RUN="$TMP/run"
    DA_OUT="$(SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" bead.sh dep "$@" 2>&1)"
    DA_RC=$?
}
run_dep_cmd epic-edges
wantrc "epic-edges exits 0" "0" "$DA_RC"
is     "epic-edges lists the planted edge" "sp-dep-work-b sp-dep-epic" "$DA_OUT"

run_dep_cmd convert sp-dep-work-b sp-dep-epic
wantrc "convert exits 0" "0" "$DA_RC"
is     "no blocks edge remains" "" "$(blocks_of sp-dep-work-b)"
is     "the edge is now parent-child" "sp-dep-epic:parent-child" "$(types_of sp-dep-work-b)"
run_dep_cmd epic-edges
is     "epic-edges is empty after conversion" "" "$DA_OUT"

run_dep_cmd convert sp-dep-work-b sp-dep-work-a
wantrc "convert onto a non-epic is refused" "1" "$DA_RC"

tl_summary
