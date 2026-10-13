#!/usr/bin/env bash
#
# test-bead-label.sh — bead.sh label add|remove sets and drops a label, and refuses a
# partition label (or fayth:*) with bead.sh file --for named as the exit.
#
# tier: T2
# covers: spira/bead.sh bead/src/main.rs bead/src/lib.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"

echo "test-bead-label.sh"

testdb_require test-bead-label
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_label || { printf 'test-bead-label: could not build fixture database\n' >&2; exit 1; }

testdb_seed <<'JSONL'
{"id":"sp-lbl-a","title":"label target","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

run_label() {
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_RUN="$TMP/run"
    LB_OUT="$(SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" bead.sh label "$@" 2>&1)"
    LB_RC=$?
}

labels_of() {
    timeout 5 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(" ".join(d.get("labels") or []))'
}

echo
echo "T1: add sets the label, remove drops it"
run_label add sp-lbl-a groom-trigger
wantrc "add ok" "0" "$LB_RC"
want "label present" "groom-trigger" "$(labels_of sp-lbl-a)"
run_label remove sp-lbl-a groom-trigger
wantrc "remove ok" "0" "$LB_RC"
nowant "label gone" "groom-trigger" "$(labels_of sp-lbl-a)"

echo
echo "T2: partition labels are refused, naming file --for"
run_label add sp-lbl-a fayth:builder
wantrc "fayth:builder refused" "1" "$LB_RC"
want "names file --for" "bead.sh file --for" "$LB_OUT"
nowant "fayth:builder not set" "fayth:builder" "$(labels_of sp-lbl-a)"
run_label remove sp-lbl-a plan
wantrc "chamber partition label refused" "1" "$LB_RC"
want "plan still present" "plan" "$(labels_of sp-lbl-a)"

echo
echo "T3: unknown bead and bad usage"
run_label add sp-lbl-nope groom-trigger
wantrc "unknown bead refused" "1" "$LB_RC"
run_label add sp-lbl-a
wantrc "usage error" "2" "$LB_RC"
