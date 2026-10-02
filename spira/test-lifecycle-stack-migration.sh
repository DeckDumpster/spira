#!/usr/bin/env bash
#
# test-lifecycle-stack-migration.sh — lifecycle/migrations/0001-stack.sql, the ALTER TABLE
# path onto a bead store that predates the stack/stack_depth columns, is otherwise only ever
# exercised via schema.sql's fresh-install CREATE TABLE (sp-s9675.7: audit found no suite
# running the migration file at all).
#
# WHAT THIS PROVES: a store built by stripping those two columns out of schema.sql (a
# stand-in for the real pre-stack database, since no earlier schema.sql survives to fixture
# from) and then migrated with 0001-stack.sql ends up with the identical `bead` table a fresh
# schema.sql install produces — same columns, same defaults on an existing row. POSITIVE
# CONTROL: the pre-migration fixture is checked to genuinely lack the columns first, so the
# match afterward is the migration's doing, not an accident of a fixture built from the
# post-stack schema already. The migration's own claim of not being idempotent (comment,
# 0001-stack.sql) is pinned too: a second run must fail on a duplicate column, not no-op.
#
# host-reason: embedded `dolt --data-dir`, no sql-server — same shape as test-schema-apply.sh.
#
# defect: sp-s9675.7
# tier: T1
# covers: lifecycle/migrations/0001-stack.sql lifecycle/schema.sql
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH"

REPO="$(cd "$HERE/.." && pwd -P)"
SCHEMA="$REPO/lifecycle/schema.sql"
MIGRATION="$REPO/lifecycle/migrations/0001-stack.sql"
[ -f "$SCHEMA" ] || bail "missing $SCHEMA"
[ -f "$MIGRATION" ] || bail "missing $MIGRATION"

T_FRESH="$(mktemp -d)"
T_PRE="$(mktemp -d)"
cleanup() { rm -rf "$T_FRESH" "$T_PRE"; }
trap cleanup EXIT INT TERM

fresh() { "$DOLT_BIN" --data-dir "$T_FRESH" sql "$@"; }
pre()   { "$DOLT_BIN" --data-dir "$T_PRE" sql "$@"; }
# 0001-stack.sql's own documented invocation is `--use-db spira_lifecycle sql < ...` (it
# carries no `USE` statement of its own, unlike schema.sql) — matched here, not just for
# schema.sql, which also runs fine either way.
pre_use() { "$DOLT_BIN" --data-dir "$T_PRE" --use-db spira_lifecycle sql "$@"; }
# Column name/type/nullability/default, sorted by name rather than read off `show create
# table` — ALTER TABLE ADD COLUMN always appends at the end, so the migrated table's physical
# column order differs from a fresh CREATE TABLE's even when the two are equivalent.
bead_shape() {
    "$1" -q "use spira_lifecycle; select column_name, column_type, is_nullable, column_default
              from information_schema.columns
              where table_schema='spira_lifecycle' and table_name='bead'
              order by column_name;" -r json 2>/dev/null
}

( cd "$T_FRESH" && "$DOLT_BIN" init -b main >/dev/null 2>&1 )
( cd "$T_PRE" && "$DOLT_BIN" init -b main >/dev/null 2>&1 )

echo
echo "a fresh schema.sql install is the baseline the migration must reproduce"
fresh < "$SCHEMA" >"$T_FRESH/apply.log" 2>&1
wantrc "schema.sql applies cleanly to a bare store" 0 $?

# The stand-in pre-stack fixture: schema.sql with the two stack columns' own definition
# lines removed. Everything else — every other table, every other column of bead — is the
# real schema.sql, so this is not a hand-maintained copy that could drift from it.
PRE_SQL="$T_PRE/pre-stack.sql"
grep -v -e 'stack       JSON NOT NULL DEFAULT (JSON_OBJECT()),' \
        -e 'stack_depth BIGINT NOT NULL DEFAULT 0,' \
        "$SCHEMA" > "$PRE_SQL"

echo
echo "the pre-stack fixture (positive control — proves it is genuinely pre-stack)"
pre < "$PRE_SQL" >"$T_PRE/apply.log" 2>&1
wantrc "the stripped-down schema applies cleanly" 0 $?
shape_before="$(bead_shape pre)"
nowant "the pre-migration bead table has no stack column" '"stack"' "$shape_before"
nowant "the pre-migration bead table has no stack_depth column" '"stack_depth"' "$shape_before"

echo
echo "running 0001-stack.sql against the pre-stack fixture"
pre_use < "$MIGRATION" >"$T_PRE/migration.log" 2>&1
wantrc "the migration applies cleanly" 0 $?

echo
echo "the migrated store now matches a fresh install's bead table exactly"
shape_fresh="$(bead_shape fresh)"
shape_migrated="$(bead_shape pre)"
is "migrated bead columns == fresh-install bead columns" "$shape_fresh" "$shape_migrated"

echo
echo "defaults on an existing row match between the two paths"
fresh -q "use spira_lifecycle; insert into bead (bead_id,state,holds,version,updated_at) values ('x','READY','[]',0,0);" >/dev/null 2>&1
pre   -q "use spira_lifecycle; insert into bead (bead_id,state,holds,version,updated_at) values ('x','READY','[]',0,0);" >/dev/null 2>&1
row_fresh="$(fresh -q "use spira_lifecycle; select stack, stack_depth from bead where bead_id='x';" -r json 2>/dev/null)"
row_migrated="$(pre -q "use spira_lifecycle; select stack, stack_depth from bead where bead_id='x';" -r json 2>/dev/null)"
is "the migrated column defaults match the fresh-install defaults" "$row_fresh" "$row_migrated"
want "and the default is the empty stack object, not null" '"stack":{}' "$row_migrated"

echo
echo "NOT IDEMPOTENT (the migration's own claim) — a second run fails, it does not no-op"
out2="$(pre_use < "$MIGRATION" 2>&1)"; rc2=$?
wantrc "re-running the migration against an already-migrated store fails" 1 $rc2
want "and names the duplicate column as why" "already exists" "$out2"

tl_summary
