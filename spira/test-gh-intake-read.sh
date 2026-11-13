#!/usr/bin/env bash
#
# test-gh-intake-read.sh — the GitHub closeout's read of the store is the work refs, not every bead.
#
# The failure this guards: gh-intake's closeout, scan and provenance read every column of every
# bead (`content list --all`) to find the few that carry a github: ref, and on a store of
# thousands that read ran into the 3 s request deadline on every pass. The read now goes through
# `plan items --external-contains github: --full`, on a real store seeded with 5,000 beads.
#
# tier: T2
# covers: gh-intake/* spira-lc/src/plan.rs spira-store/src/lc_call.rs
# timeout: 600
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH"

echo "test-gh-intake-read.sh"
. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require ghread
TMP="$(mktemp -d)"
export SPIRA_TESTDB_MODE=server
testdb_up ghread || skip "server testdb not available"
. "$HERE/lib.sh"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
. "$HERE/testlib/lc-dolt-fixture.sh"
. "$HERE/testlib/lc-fixture.sh"
trap 'lcfix_down; lcdolt_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
lcdolt_up_on_testdb || bail "the dolt lifecycle database did not come up"
lcfix_up || bail "the control store did not come up"

testdb_reset
# bd import of this many beads outruns a case deadline, so the rows go in as SQL, a hundred a statement.
python3 - "$TMP" <<'PY'
import sys
body = "x" * 400
rows = []
for i in range(5000):
    ext, reason, closed = "NULL", "NULL", "NULL"
    if i % 500 == 0: ext = f"'github:fixture/repo#{i}'"
    if i % 500 == 250: ext = f"'github-untrusted:fixture/repo#{i}'"
    status = "closed" if i % 2 else "open"
    if i == 500: status, reason, closed = "closed", "'landed in a test'", "'2026-09-02 03:04:05'"
    rows.append(f"('sp-r{i:04d}','bead {i}','{body}','{body}','','{body}','{status}',2,'task',{ext},{reason},{closed},NOW(),NOW())")
for n in range(0, len(rows), 100):
    open(f"{sys.argv[1]}/seed-{n // 100:03d}.sql", "w").write(
        "INSERT INTO issues (id,title,description,design,acceptance_criteria,notes,status,priority,issue_type,external_ref,close_reason,closed_at,created_at,updated_at) VALUES " + ",".join(rows[n:n + 100]))
PY
for f in "$TMP"/seed-*.sql; do
    "$SPIRA_BD" -C "$SPIRA_DB" sql "$(cat "$f")" >/dev/null 2>"$TMP/seed.err" || bail "could not seed 5,000 beads: $(grep -v "^Warning" "$TMP/seed.err" | head -c 400)"
done
"$SPIRA_BD" -C "$SPIRA_DB" sql "INSERT INTO labels (issue_id,label) SELECT id,'spira' FROM issues" >/dev/null 2>&1 || bail "could not label the beads"

ms() { local a b; a=$(date +%s%N); "$@" > "$TMP/out" 2> "$TMP/err"; b=$(date +%s%N); echo $(( (b - a) / 1000000 )); }

t=$(ms spira-lc plan items --external-contains "github:" --full)
echo "# narrow read: ${t} ms"
[ "$t" -lt 500 ] && ok "the closeout's read of 5,000 beads takes under 500 ms" || bad "the closeout's read of 5,000 beads takes under 500 ms" "${t} ms"
n=$(python3 -c 'import sys,json; print(len(json.load(open(sys.argv[1]))["items"]))' "$TMP/out")
is "only the work refs come back, not the untrusted records or the other beads" "10" "$n"
row=$(python3 -c 'import sys,json
for i in json.load(open(sys.argv[1]))["items"]:
    if i["id"]=="sp-r0500": print(i.get("close_reason"), i.get("closed_at"))' "$TMP/out")
is "the row carries the close reason and closing time the closeout reads" "landed in a test 2026-09-02T03:04:05Z" "$row"

tl_summary
