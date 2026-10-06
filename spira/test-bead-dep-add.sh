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
    "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" dep list "$1" --type blocks --json 2>/dev/null \
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

tl_summary
