#!/usr/bin/env bash
#
# test-content-sql.sh — `spira-lc content` reads are SQL against the beads database: each read
# verb and filter the callers use, against a real bd store on a throwaway Dolt server, and no bd
# process for any of them. A store whose layout the reads do not know is refused by name.
#
# host-reason: starts its own disposable `dolt sql-server` (the beads store is a dolt database).
#
# tier: T2
# covers: spira-lc/src/content_sql.rs spira-lc/src/content_sql/* spira-lc/src/content.rs spira-lc/src/composed.rs spira-lc/src/plan.rs spira-lc/src/db.rs
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
command -v dolt >/dev/null 2>&1 || skip "dolt not found on PATH"
. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require content-sql
TMP="$(mktemp -d)"
# testdb-mode: server — the reads are SQL over the MySQL protocol; embedded mode has no server.
export SPIRA_TESTDB_MODE=server
testdb_up content-sql || skip "server testdb not available"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

BD="${TESTDB_SERVER_BD:-bd}"
port="$(cat "$SPIRA_DB/.beads/dolt-server.port" 2>/dev/null)" || bail "the testdb names no server port"
: > "$TMP/credential"
# A bd that records its own start: a content read must never reach it.
cat > "$TMP/bd-forked" <<EOF
#!/usr/bin/env bash
echo "\$@" >> "$TMP/bd-forks"
exit 1
EOF
chmod +x "$TMP/bd-forked"
tl_config SPIRA_LC_SOCKET="" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$TMP/bd-forked" \
    SPIRA_BEADS_SQL_HOST=127.0.0.1 SPIRA_BEADS_SQL_PORT="$port" SPIRA_BEADS_SQL_USER=root SPIRA_BEADS_SQL_PASSWORD_FILE="$TMP/credential" \
    || bail "cannot declare the connection"

mk() { timeout 5 "$BD" -C "$SPIRA_DB" create "$@" --silent 2>/dev/null; }
epic="$(mk "The epic" -t epic)"
a="$(mk "Alpha bead" -t task -p 1 --labels tag-one,repo:spira --description "alpha body")"
b="$(mk "Beta bead" -t bug -p 2 --labels tag-two)"
c="$(mk "Gamma bead" -t task -p 3 --parent "$epic" --external-ref "incident:gamma")"
d="$(mk "Delta bead" -t task -p 2)"
[ -n "$epic" ] && [ -n "$a" ] && [ -n "$b" ] && [ -n "$c" ] && [ -n "$d" ] || bail "could not seed the beads"
timeout 5 "$BD" -C "$SPIRA_DB" dep add "$a" "$b" >/dev/null 2>&1
timeout 5 "$BD" -C "$SPIRA_DB" close "$d" --reason done >/dev/null 2>&1
timeout 5 "$BD" -C "$SPIRA_DB" comments add "$a" "a remark" >/dev/null 2>&1
timeout 5 "$BD" -C "$SPIRA_DB" remember "statute one: be good" >/dev/null 2>&1
gate="$(mk "A gate" -t gate)"

rd() { spira-lc content "$@" 2>"$TMP/err"; }
ids() { python3 -c 'import sys,json; print(" ".join(sorted(r["id"] for r in json.load(sys.stdin))))'; }
want_ids() { is "$1" "$(printf '%s\n' "$2" | tr ' ' '\n' | sort | tr '\n' ' ' | sed 's/ $//')" "$3"; }

# list --label-any
out="$(rd list --limit 0 --label-any tag-one,tag-two --json | ids)"
want_ids "list --label-any selects the beads carrying either label" "$a $b" "$out"
out="$(rd list --limit 0 --label-any nobody-has-this --json | ids)"
is "list --label-any with a label no bead has selects nothing" "" "$out"
# list --id
out="$(rd list --all --limit 0 --json --id "$a,$d" | ids)"
want_ids "list --id selects exactly those ids, closed included" "$a $d" "$out"
# list default and --all
out="$(rd list --limit 0 --json | ids)"
want_ids "list is the open beads, with no gate" "$epic $a $b $c" "$out"
out="$(rd list --all --limit 0 --json | ids)"
want_ids "list --all adds the closed bead" "$epic $a $b $c $d" "$out"
out="$(rd list --limit 2 --json | ids | wc -w)"
is "list --limit bounds the rows" "2" "$(echo $out)"
# --parent, --label (AND), --external-ref, --status, --closed-after
out="$(rd list --limit 0 --parent "$epic" --json | ids)"
is "list --parent selects the children" "$c" "$out"
out="$(rd list --limit 0 --label tag-one,repo:spira --json | ids)"
is "list --label requires every label" "$a" "$out"
out="$(rd list --limit 0 --label tag-one,tag-two --json | ids)"
is "list --label with labels no one bead has selects nothing" "" "$out"
out="$(rd list --all --limit 0 --external-ref incident:gamma --json | ids)"
is "list --external-ref is an exact match" "$c" "$out"
out="$(rd list --all --limit 0 --status closed --json | ids)"
is "list --status closed selects the closed bead" "$d" "$out"
out="$(rd list --all --limit 0 --closed-after 2020-01-01 --json | ids)"
is "list --closed-after selects a bead closed since" "$d" "$out"
out="$(rd list --all --limit 0 --closed-after 2999-01-01 --json | ids)"
is "list --closed-after a future date selects nothing" "" "$out"
# the row
row="$(rd show "$a" --json)"
want "show carries the description" '"description":"alpha body"' "$row"
want "show carries the labels as an array" '"labels":["repo:spira","tag-one"]' "$row"
want "show carries the dependency edge" "\"depends_on_id\":\"$b\"" "$row"
brief="$(rd list --limit 0 --brief --label-any tag-one --json)"
nowant "list --brief leaves out the description" "alpha body" "$brief"
rd show nope-1 --json >/dev/null; rc=$?
is "show of an unknown bead exits 1" "1" "$rc"
want "show of an unknown bead says so" "no issue found matching" "$(cat "$TMP/err")"
want "gate list lists the gate" "$gate" "$(rd gate list --json)"
want "memories are keyed by name" '"statute-one-be-good":"statute one: be good"' "$(rd memories --json)"
want "comments are the comment rows" '"text":"a remark"' "$(rd comments "$a" --json)"
is "state reads a dimension label" "spira" "$("spira-lc" content state "$a" repo 2>/dev/null | tr -d '\n')"
is "config get reads the store's config" "sp" "$(rd config get issue_prefix | tr -d '\n')"

# A write that does not come through the door is seen by the next read: nothing is held.
timeout 5 "$BD" -C "$SPIRA_DB" label add "$b" fresh-label >/dev/null 2>&1
out="$(rd list --limit 0 --label-any fresh-label --json | ids)"
is "a label written by another process is selected at once" "$b" "$out"

is "no content read started bd" "no" "$([ -s "$TMP/bd-forks" ] && echo yes || echo no)"

# A layout the reads do not know is refused by name, and bd is still not started.
timeout 5 "$BD" -C "$SPIRA_DB" sql "ALTER TABLE labels RENAME COLUMN label TO tag" >/dev/null 2>&1 || bail "could not rename the column"
out="$(rd list --limit 0 --json)"; rc=$?
is "a renamed column refuses the read with exit 2" "2" "$rc"
err="$(cat "$TMP/err")"
want "the refusal names the missing column" "labels.label" "$err"
want "the refusal names the exit" "exit 2" "$err"
is "a refused read printed no rows" "" "$out"
is "a refused read did not start bd" "no" "$([ -s "$TMP/bd-forks" ] && echo yes || echo no)"

tl_summary
