#!/usr/bin/env bash
#
# test-incident-recur-bounded.sh — recurrence notes are bounded when payload is unchanged
#
# covers: spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
le()  { [ "$3" -le "$2" ] && ok "$1" || bad "$1" "wanted <= $2, got $3"; }
ge()  { [ "$3" -ge "$2" ] && ok "$1" || bad "$1" "wanted >= $2, got $3"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident-recur-bounded
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up incident-recur-bounded || { echo "test-incident-recur-bounded: could not build fixture database"; exit 1; }

# shellcheck disable=SC1090
. "$HERE/lib.sh"
INC="$HERE/incident.sh"
B() { bd -C "$SPIRA_DB" "$@"; }
mkdir -p "$TMP/run"

NOOP="$TMP/noop.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$NOOP"; chmod +x "$NOOP"

file_incident() {
    local ref="$1" title="$2" payload="$3"; shift 3
    printf '%s' "$payload" | \
        env SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" \
        SPIRA_INCIDENT_REF="$ref" SPIRA_NOTIFY="$NOOP" \
        SPIRA_INCIDENT_LOCK="$TMP/run/recur-bounded-test.lock" \
        SPIRA_INCIDENT_CAUSE=suite-red \
        SPIRA_INCIDENT_REPO= \
        "$@" \
        bash "$INC" file "$title" - 2>/dev/null
}

notes_length() {
    B show "$1" --json 2>/dev/null \
      | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    d = d if isinstance(d, list) else [d]
    n = (d[0].get("notes") or "") if d else ""
    print(len(n.encode("utf-8")))
except: print(0)
'
}

notes_content() {
    B show "$1" --json 2>/dev/null \
      | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    d = d if isinstance(d, list) else [d]
    print((d[0].get("notes") or "") if d else "")
except: print("")
'
}

find_bead() {
    B list --status open,in_progress --limit 0 --label spira,partition:incident --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try:
    for b in json.load(sys.stdin):
        if b.get("external_ref") == target:
            print(b["id"]); raise SystemExit(0)
except SystemExit: raise
except: pass
print("")
' "$1"
}

PAYLOAD="$(python3 -c 'print("x" * 500, end="")')"   # 500-byte fixed payload
N=20

echo "test-incident-recur-bounded.sh"

# ======================================================================================
echo
echo "positive control — direct note appends produce measurable notes (validates notes_length):"
# ======================================================================================
# A notes_length that silently returns 0 would make the bounded-notes assertion pass
# vacuously. Plant $((N-1)) long notes directly and require >= $((N-1)) * 500 bytes.
REF_PC="incident:test-recur-bounded-posctrl"
testdb_reset; mkdir -p "$TMP/run"
_pc_id="$(B create "recur-bounded positive control" --type bug --priority 2 \
    --labels spira,partition:incident --external-ref "$REF_PC" --silent 2>/dev/null \
    | tr -d '[:space:]')"
[ -n "$_pc_id" ] || { bad "positive control: bead created" "create failed"; \
    printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }
ok "positive control: bead created"

for _i in $(seq 1 $((N-1))); do
    B note "$_pc_id" "Recurrence $_i at 2026-09-16T00:00:00Z.
$PAYLOAD" >/dev/null 2>&1
done
_pc_len="$(notes_length "$_pc_id")"
_pc_min=$(( (N-1) * 500 ))
ge "positive control: $((N-1)) long notes produce >= $((N-1)) * 500 bytes (got $_pc_len)" \
    "$_pc_min" "$_pc_len"

# ======================================================================================
echo
echo "bounded notes — $N identical filings grow notes by <=120 bytes per recurrence:"
# ======================================================================================
REF="incident:test-recur-bounded-same"
testdb_reset; mkdir -p "$TMP/run"
for _i in $(seq 1 $N); do
    file_incident "$REF" "recur bounded test" "$PAYLOAD" >/dev/null
done

_bid="$(find_bead "$REF")"
[ -n "$_bid" ] || { bad "bounded-notes bead was created" "none found"; \
    printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }
ok "bounded-notes bead was created"

_notes_len="$(notes_length "$_bid")"
_max_notes=$(( (N-1) * 120 ))
le "$((N-1)) recurrences produce notes <= $((N-1)) * 120 bytes (got $_notes_len, max $_max_notes)" \
    "$_max_notes" "$_notes_len"

# ======================================================================================
echo
echo "changed payload — a changed payload is recorded in full:"
# ======================================================================================
# If the fix simply stopped writing notes, this assertion would fail — proving the
# change-detection path is wired.
PAYLOAD_A="$(python3 -c 'print("a" * 400, end="")')"
PAYLOAD_B="$(python3 -c 'print("b" * 400, end="")')"
REF2="incident:test-recur-bounded-changed"
testdb_reset; mkdir -p "$TMP/run"
for _i in $(seq 1 5); do
    file_incident "$REF2" "recur bounded changed test" "$PAYLOAD_A" >/dev/null
done
file_incident "$REF2" "recur bounded changed test" "$PAYLOAD_B" >/dev/null

_bid2="$(find_bead "$REF2")"
[ -n "$_bid2" ] || { bad "changed-payload bead was created" "none found"; \
    printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }
ok "changed-payload bead was created"

_nc="$(notes_content "$_bid2")"
[[ "$_nc" == *"$PAYLOAD_B"* ]] \
    && ok "changed payload B is recorded in notes" \
    || bad "changed payload B is recorded in notes" "notes length=${#_nc}"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
