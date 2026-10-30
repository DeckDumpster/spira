#!/usr/bin/env bash
#
# test-fixture-store-error.sh — a fixture's lifecycle stand-in answers "cannot tell" when its
# store fails, never an empty ready set; and a seed is checked visible before a claim runs.
#
# A dwarf in a fixture claims through the stand-in `spira-lc list`. A bd that failed under
# load used to read as `[]`, exit 0, which the dwarf reported as "nothing ready to claim"
# with a READY bead sitting in the store. The dwarf already turns a failed ready read into
# claim-error (dwarf/src/tests.rs); this guards the stand-in that feeds it.
#
# tier: T1
# covers: spira/testlib.sh spira/testdb.sh dwarf/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
echo "test-fixture-store-error.sh"

mkdir -p "$T/run" "$T/lcm"
lc_dwarf_mirror "$T/lcm"

# A bd whose answer to `list` is whatever $T/mode says.
cat > "$T/bd" <<'STUB'
#!/usr/bin/env bash
case "$(cat "$T/mode")" in
    ok) printf '[{"id":"sp-fx-1","status":"open","issue_type":"task","labels":["plan"]}]\n' ;;
    other) printf '[{"id":"sp-fx-other","status":"open","issue_type":"task","labels":[]}]\n' ;;
    fail) echo "Error: database is locked" >&2; exit 1 ;;
    empty) ;;
    garbled) printf '{"id": ' ;;
esac
STUB
chmod +x "$T/bd"
export T

lc() { PATH="$T/lcm:$PATH" SPIRA_BD="$T/bd" SPIRA_DB="$T" SPIRA_RUN="$T/run" spira-lc "$@" 2>/dev/null; }

echo "positive control: a store that answers is read as READY"
echo ok > "$T/mode"
out="$(lc list --state READY)"; rc=$?
wantrc "list answers"  0 "$rc"
want   "the READY row is listed"  sp-fx-1 "$out"

for mode in fail empty garbled; do
    echo
    echo "a store that gives $mode is cannot-tell, not an empty ready set"
    echo "$mode" > "$T/mode"
    out="$(lc list --state READY)"; rc=$?
    wantrc "list exits cannot-tell ($mode)"  2 "$rc"
    nowant "no empty array handed over ($mode)"  "[]" "$out"
done

echo
echo "the seed check: ids the import carried must be readable"
printf '{"id":"sp-fx-1","title":"t"}\n' > "$T/seed.jsonl"
export SPIRA_BD="$T/bd" SPIRA_DB="$T" TESTDB_SEED_TRIES=1
echo ok > "$T/mode";    _testdb_seed_visible "$T/seed.jsonl" 2>/dev/null; wantrc "a visible seed passes" 0 "$?"
echo other > "$T/mode"; _testdb_seed_visible "$T/seed.jsonl" 2>/dev/null; wantrc "a missing seed is refused" 1 "$?"
echo fail > "$T/mode";  _testdb_seed_visible "$T/seed.jsonl" 2>/dev/null; wantrc "an unreadable store is refused" 1 "$?"

tl_summary
