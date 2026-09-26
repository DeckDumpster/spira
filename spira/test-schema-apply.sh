#!/usr/bin/env bash
#
# test-schema-apply.sh — the declared model is in the store, and its ABSENCE is detected.
#
#   ./test-schema-apply.sh
#
# WHAT THIS IS REALLY GUARDING. Two pieces of the model live in Dolt, below bd
# (law-schema-over-code): the _is_work generated column and the spira_priority_range CHECK.
# They are modifications to an EXTERNAL DEPENDENCY's schema — bd carries its own
# schema_migrations and an --ignore-schema-skew flag, so an upgrade can rewrite a table
# without carrying them across.
#
# A CONSTRAINT THAT SILENTLY DISAPPEARED IS WORSE THAN ONE NEVER ADDED, because every tool
# above it stopped guarding what it no longer does. So the assertion that matters is not
# "the constraint works" — it is "the detection NOTICES when it is gone".
#
# THAT NEGATIVE IS PROVED IN A FIXTURE, NOT IN PRODUCTION. An earlier version of this suite
# dropped the real constraint, asserted, and added it back: a window in which production had
# no constraint, and if the suite died between the two it stayed gone — the exact failure it
# exists to detect, caused by the detector (law-probe-a-fixture-not-production). The fixture
# is a throwaway Dolt database in the suite's own scratch directory; it costs ~0.3s.
#
# NOTHING HERE TOUCHES THE CONFIGURED STORE ANY MORE. Two assertions used to: one ran
# `schema.sh check` against it, and one ran `schema-apply.sh` with no arguments — which is a
# WRITE to the operator's real database, in a suite whose own header promised production was
# read-only. Both passed on a box whose store was already correct and failed everywhere else;
# containerizing the timed pass surfaced them at once as
#   Error: cannot use -C directory ".../db": no such file or directory
# They were also asserting the wrong thing. "Is this box's store correct" is a question about
# one machine and belongs to doctor.sh; what a suite must pin is that APPLY MAKES A STORE
# CORRECT AND IS IDEMPOTENT, which is a question about the code and is answerable on a bare
# fixture. So apply now runs twice against a throwaway from testdb.sh: the first run is the
# positive control that it changes something, the second that it changes nothing.
#
# tier: T2
# covers: spira/schema.sh spira/schema-apply.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/conf.sh" 2>/dev/null || true


T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
echo "test-schema-apply.sh"

# shellcheck source=/dev/null
. "$HERE/testdb.sh"
testdb_require "test-schema-apply.sh"

echo
echo "the generated column classifies every declared kind"
work_types="$(awk -F'"' '/^SCHEMA_WORK_TYPES=/{print $2}' "$HERE/schema.sh")"
for k in $("$HERE/schema.sh" kinds); do
    t="$("$HERE/schema.sh" type-of "$k")"
    case " $work_types " in
        *" $t "*) [ "$k" = work ] || [ "$t" = chore ] && ok "kind $k ($t) is a work type" || bad "kind $k" "unexpectedly a work type" ;;
        *)        ok "kind $k ($t) is excluded from work by type" ;;
    esac
done

echo
echo "NEGATIVE — detection notices a missing CHECK (proved in a fixture, never in production)"
( cd "$T" && dolt init -b main >/dev/null 2>&1 )
dolt --data-dir "$T" sql -q "create database fx; use fx; create table issues (id varchar(64) primary key, priority int, issue_type varchar(32));" >/dev/null 2>&1
q="select count(*) as n from information_schema.table_constraints where table_name='issues' and constraint_type='CHECK' and constraint_name='spira_priority_range';"
fx(){ dolt --data-dir "$T" sql -q "use fx; $1" 2>/dev/null | sed -n '4p' | tr -d '| '; }

[ "$(fx "$q")" = "0" ] && ok "absent constraint detected as absent" || bad "absent" "got [$(fx "$q")]"
dolt --data-dir "$T" sql -q "use fx; alter table issues add constraint spira_priority_range check (priority between 0 and 4);" >/dev/null 2>&1
[ "$(fx "$q")" = "1" ] && ok "control — present constraint detected as present" || bad "present" "got [$(fx "$q")]"
dolt --data-dir "$T" sql -q "use fx; alter table issues drop constraint spira_priority_range;" >/dev/null 2>&1
[ "$(fx "$q")" = "0" ] && ok "dropped constraint detected as gone — the bd-upgrade scenario" || bad "dropped" "got [$(fx "$q")]"

echo
echo "and the constraint actually refuses, in the fixture"
dolt --data-dir "$T" sql -q "use fx; alter table issues add constraint spira_priority_range check (priority between 0 and 4);" >/dev/null 2>&1
out="$(dolt --data-dir "$T" sql -q "use fx; insert into issues values ('x',9,'task');" 2>&1)"
want "priority 9 refused by the CHECK" "spira_priority_range" "$out"
out="$(dolt --data-dir "$T" sql -q "use fx; insert into issues values ('y',2,'task');" 2>&1)"
[[ "$out" != *"violated"* ]] && ok "control — priority 2 accepted" || bad "control" "good write refused: $out"

# ======================================================================================
echo
echo "on an engine that cannot run bd sql, apply says so instead of reporting drift"
# ======================================================================================
# bd in embedded mode answers every query with
#   Error: 'bd sql' is not yet supported in embedded mode
# so both existence probes came back empty, both DDL statements were attempted and refused
# with their output discarded, and the run ended announcing "applied" and then listing the
# column and constraint as MISSING. Nothing in that output says the engine was never able to
# be asked. This pins the honest failure: named reason, non-zero status, no "applied".
if ! testdb_up schema-apply-embedded >/dev/null 2>&1; then
    bad "embedded fixture" "testdb_up failed — the engine-limitation path is UNTESTED"
elif [ "${TESTDB_MODE:-embedded}" != embedded ]; then
    printf 'note: fixture came up in %s mode; the embedded-engine assertions did not run\n' \
        "$TESTDB_MODE" >&2
    testdb_drop >/dev/null 2>&1
else
    o_emb="$("$HERE/schema-apply.sh" 2>&1)"; rc_emb=$?
    [ "$rc_emb" != 0 ] && ok "apply exits non-zero on an engine it cannot use (rc=$rc_emb)" \
                       || bad "apply exits non-zero on an engine it cannot use" \
                              "it exited 0 and the caller has no way to know: $o_emb"
    want "and names the engine as the reason"     "EMBEDDED mode"  "$o_emb"
    want "and names what could not be applied"    "_is_work"       "$o_emb"
    want "and says where it could be applied"     "server-mode"    "$o_emb"
    # THE ASSERTION THAT MATTERS MOST: it must not claim success.
    case "$o_emb" in
        *"applied. verifying"*) bad "and does not claim it applied anything" \
                                    "the word 'applied' is still in the output" ;;
        *) ok "and does not claim it applied anything" ;;
    esac
    testdb_drop >/dev/null 2>&1
fi

# ======================================================================================
echo
echo "apply makes a bare store correct, and a second apply changes nothing"
# ======================================================================================
# THE SQL HALF NEEDS A SERVER-MODE STORE, for the reason the section above pins. Where one is
# not reachable this cannot run, and it says so on stderr rather than passing quietly — the
# section above is what still holds on such a box, and it is an assertion about this code,
# not an absence (law-absence-needs-a-positive-control).
# A SERVER FIXTURE THAT WILL NOT COME UP IS NOT A FAILURE OF schema-apply.sh, and must not be
# reported as one — this suite's subject is the applier, and a red here would name the wrong
# program. It is also not nothing: the gap is stated loudly on stderr, which suites.sh carries
# into the bead body, so what did not get covered is on the record rather than implied by a
# count (law-alerts-must-be-actionable, law-absence-needs-a-positive-control).
# NOT IN A COMMAND SUBSTITUTION. testdb_up EXPORTS SPIRA_DB, and `$( ... )` runs it in a
# subshell where that export dies with the subshell — leaving SPIRA_DB pointing at the
# embedded fixture the section above already dropped, so every statement below failed with
# "cannot use -C directory ...: no such file or directory". Its stderr goes to a file
# instead, which is the only thing the substitution was buying.
# testdb-mode: server — the generated column and CHECK constraint are applied through
# bd sql, which embedded mode refuses; the section above already pins that refusal.
_srv_err=""
_srv_log="$T/server-fixture.log"
if [ -z "${SPIRA_TESTDB_DATA:-}" ]; then
    _srv_err="SPIRA_TESTDB_DATA is unset — there is no server-mode fixture on this box"
elif ! SPIRA_TESTDB_MODE=server testdb_up schema-apply-server >/dev/null 2>"$_srv_log"; then
    _srv_err="$(cat "$_srv_log" 2>/dev/null)"
    _srv_err="${_srv_err:-testdb_up failed with no output}"
fi
if [ -n "$_srv_err" ]; then
    printf '\n' >&2
    printf 'NOT COVERED HERE: the SQL half of the model — the _is_work generated column,\n' >&2
    printf '  the spira_priority_range CHECK, and whether a second apply is a no-op. Those\n' >&2
    printf '  need a server-mode store, because bd cannot run SQL in embedded mode.\n' >&2
    printf '  The server fixture did not come up:\n' >&2
    printf '%s\n' "$_srv_err" | sed 's/^/    /' >&2
    printf '  The embedded assertions above did run and still hold.\n\n' >&2
else
    # FIRST APPLY IS THE POSITIVE CONTROL. A bare fixture has none of the model, so this run
    # must report that it ADDED things. Without it, the idempotence assertions below would
    # pass just as well against an apply that had quietly become a no-op on every store.
    o1="$("$HERE/schema-apply.sh" 2>&1)"; rc1=$?
    [ "$rc1" = 0 ] && ok "apply succeeds on a bare store" \
                   || bad "apply succeeds on a bare store" "exited $rc1: $o1"
    want "and adds the generated column"    "_is_work"              "$o1"
    want "and adds the priority constraint" "spira_priority_range"  "$o1"

    # NOW the store is correct, so the checker must say so. This is the assertion that used
    # to be made against the operator's own database.
    "$HERE/schema.sh" check >/dev/null 2>&1 \
        && ok "schema.sh check passes against the store apply just built" \
        || bad "schema.sh check" "reported drift on a store schema-apply.sh had just applied"

    o2="$("$HERE/schema-apply.sh" 2>&1)"
    want "second run reports types already exact"    "already exact"   "$o2"
    want "second run reports _is_work already there" "already present" "$o2"
    testdb_drop >/dev/null 2>&1
fi

echo
tl_summary
