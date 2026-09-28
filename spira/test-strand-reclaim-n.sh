#!/usr/bin/env bash
#
# test-strand-reclaim-n.sh — cmd_check's ghost-reclaim path survives set -u.
#
#   ./test-strand-reclaim-n.sh
#
# WHY THIS EXISTS. sp-sa4mi: cmd_check's ghost branch read a bare $n (twice, "reclaim $n")
# that is only ever assigned inside cmd_report, a function that never runs during `check`.
# Under this file's own `set -uo pipefail`, the unbound expansion is a shell error raised
# before the command runs — not rescued by the trailing `|| true` — and bash exits the
# whole process. One ghost row killed the pass before every later row in the same run was
# even looked at. Verified against the unfixed tree: with two ghost rows in the fixture,
# only the first RECLAIMED line was ever printed, "n: unbound variable" reached stderr, and
# the run — captured via command substitution, which reports the child's unbound-variable
# death as rc=1 — exited nonzero. Any one of those three failing is the bug's signature;
# all three failing together is what this suite requires.
#
# tier: T1
# defect: sp-sa4mi
# covers: spira/strand.sh
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-strand-reclaim-n
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up strandreclaimn || { echo "test-strand-reclaim-n: could not build fixture database"; exit 1; }

mkdir -p "$TMP/run" "$TMP/home"

# mail.sh stub — the two rows below never reach escalation (RECLAIM_AT defaults to 5, well
# above the single reclaim each row here earns), but a stub is cheap insurance against a
# real send if that ever changes.
cat > "$TMP/home/mail.sh" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
chmod +x "$TMP/home/mail.sh"

# TWO ghost rows, disposition "act" — the branch that reads $n. Neither id needs to be a
# real bead: bump_reclaim writes a raw events-table row keyed on the id string, and the
# reclaim/note calls that precede it are backgrounded with `|| true`/redirected output, so a
# nonexistent id is silent noise, not a failure. What matters is that BOTH rows are reached.
printf '%s\n%s\n' \
       $'ghost\tsp-fakeg1\tact\tno live aeon holds sp-fakeg1\tbdq reclaim --id sp-fakeg1' \
       $'ghost\tsp-fakeg2\tact\tno live aeon holds sp-fakeg2\tbdq reclaim --id sp-fakeg2' \
    > "$TMP/fixture.tsv"

echo "test-strand-reclaim-n.sh"

echo
echo "case 1 — cmd_check processes every ghost row without dying mid-pass:"
out="$(env SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}" SPIRA_RUN="$TMP/run" \
           SPIRA_STRAND_GRACE=0 SPIRA_LABELS=- SPIRA_HOME="$TMP/home" \
       bash "$HERE/strand.sh" check --from "$TMP/fixture.tsv" 2>"$TMP/err")"
rc=$?
wantrc "cmd_check exits 0" "0" "$rc"
want   "first ghost row reclaimed"  "RECLAIMED sp-fakeg1"  "$out"
want   "second ghost row reclaimed — proves the pass survived the first" \
       "RECLAIMED sp-fakeg2"  "$out"
nowant "no unbound-variable error reaches stderr" "unbound variable" "$(cat "$TMP/err" 2>/dev/null)"

tl_summary
