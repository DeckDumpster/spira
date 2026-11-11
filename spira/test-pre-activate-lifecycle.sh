#!/usr/bin/env bash
# tier: T2
# covers: spira/pre-activate.sh spira-lc/src/migrate.rs lifecycle/migrations/* lifecycle/grants.sql
#
# test-pre-activate-lifecycle.sh — the lifecycle step of pre-activate applies the release's grants
# on every activation, whether or not a migration file changed, and refuses the release when the
# grant step cannot vouch for them.
#
# defect: sp-carnba
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-pre-activate-lifecycle.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

fn() { awk -v n="$1" '$0 ~ "^"n"\\(\\) \\{" {p=1} p {print} p && /^}/ {exit}' "$HERE/pre-activate.sh"; }
FUNCS="$(fn check_lifecycle; fn lifecycle_grants)"
[ -n "$FUNCS" ] && ok "the lifecycle functions are found in pre-activate.sh (positive control)" || bad "no lifecycle functions found"

# A releases dir with a live release and a candidate whose migration files are byte-identical.
mkdir -p "$TMP/rel/live/lifecycle/migrations" "$TMP/rel/new/lifecycle/migrations" "$TMP/rel/new/bin"
echo "SELECT 1;" | tee "$TMP/rel/live/lifecycle/migrations/0001-a.sql" > "$TMP/rel/new/lifecycle/migrations/0001-a.sql"
ln -s live "$TMP/rel/current"
cat > "$TMP/rel/new/bin/spira-lc" <<'LC'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "$LC_CALLS"
if [ "$1" = admin-grants ] && [ -n "${LC_GRANTS_REFUSAL:-}" ]; then
    printf '%s\n' "$LC_GRANTS_REFUSAL"
    exit 2
fi
echo "$1 done"
LC
chmod +x "$TMP/rel/new/bin/spira-lc"

run_step() {  # run_step <extra env...>: prints the step's output, its stderr and "rc=<n>"
    ( env -i PATH="$PATH" LC_CALLS="$TMP/calls" "$@" bash -c '
        REL="$1"; FAIL=0
        fail() { printf "FAIL %s: %s\n" "$1" "$2" >&2; FAIL=1; }
        ok() { printf "ok %s\n" "$1"; }
        eval "$2"
        check_lifecycle
        exit "$FAIL"' _ "$TMP/rel/new" "$FUNCS" 2>&1; echo "rc=$?" )
}

: > "$TMP/calls"
out="$(run_step)"
calls="$(cat "$TMP/calls")"
want "unchanged migrations: the step passes" "rc=0" "$out"
want "unchanged migrations: the grant pass still runs" "admin-grants" "$calls"
nowant "unchanged migrations: no migration is probed" "admin-migrate" "$calls"

: > "$TMP/calls"
out="$(run_step LC_GRANTS_REFUSAL='admin-grants: spira_lc_ro cannot SELECT spira.issues after the grant')"
want "a read-only user that cannot SELECT refuses the release" "rc=1" "$out"
want "the refusal names the user and the table" "spira_lc_ro cannot SELECT spira.issues" "$out"

tl_summary
