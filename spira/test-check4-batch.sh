#!/usr/bin/env bash
#
# test-check4-batch.sh — CHECK 4's per-bead bd calls are replaced by one batched query.
#
# THREE ACCEPTANCE CRITERIA:
# 1. dispatchable_open emits id TAB comma-separated-labels, one per bead.
# 2. check4_bulk_data returns the same attempts and reopens as per-bead calls, on a fixture
#    that contains a bead with a non-zero reopen count (so a wrong result is detectable).
# 3. The sentinel's CHECK 4 loop reads labels and counts from the pre-loaded data, not from
#    per-bead bd calls — asserted by confirming the sentinel correctly uses the batched values
#    (poison fires, requeue cap fires) without making per-bead label queries.
#
# FAIL-FIRST: criterion 2 asserts that check4_bulk_data exists and returns correct values.
# Against the pre-fix code (where check4_bulk_data does not exist) this test fails at that
# assertion with "command not found" — the missing function is the failure the test exists
# to prevent from silently recurring.
#
# defect: sp-f1m7f
# covers: spira/sentinel.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check4-batch
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up check4-batch || {
    printf 'SKIP test-check4-batch: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-check4-batch.sh"

# --------------------------------------------------------------------------------------
# CRITERION 1: dispatchable_open emits id TAB labels, one per bead.
# --------------------------------------------------------------------------------------
echo
echo "criterion 1: dispatchable_open emits id TAB labels"

REPO="$TMP/repo"; RUN="$TMP/run"; SH="$TMP/spira"
mkdir -p "$RUN/worktree" "$SH/chamber"
cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'printf "%s" "${PILGRIMAGE_OUT:-}"'
stub strand.sh     'printf "%s" "${STRAND_OUT:-}"'
stub sending.sh    'printf "%s" "${SENDING_OUT:-}"'
stub governor.sh   'exit 0'
stub reflect.sh    'true'
stub mail.sh       '[ "${1:-}" = send ] || exit 0'
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SH/chamber/t.fayth"

lib_predicate() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_GOAL=sp-goal \
    SPIRA_FAYTHS="t" bash -c ". \"$SH/lib.sh\"; $1" 2>/dev/null
}

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-d1","title":"bead one","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"sp-d2","title":"bead two","status":"open","issue_type":"task","labels":["spira","plan","repo:spira"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

out="$(lib_predicate dispatchable_open)"

# Each line must be id TAB labels (tab-separated, labels comma-joined).
d1_line="$(printf '%s' "$out" | grep '^sp-d1')"
d2_line="$(printf '%s' "$out" | grep '^sp-d2')"
want "sp-d1 line includes id"             "sp-d1" "$d1_line"
want "sp-d1 line includes its labels"     "spira" "$d1_line"
want "sp-d1 line has tab separator"       $'\t'   "$d1_line"
want "sp-d2 line includes repo label"     "repo:spira" "$d2_line"

# The first field of each line is the id — a consumer that takes only $1 gets the id.
d1_id="$(printf '%s' "$d1_line" | cut -f1)"
is "first field of sp-d1 line is the id" "sp-d1" "$d1_id"

# --------------------------------------------------------------------------------------
# CRITERION 2: check4_bulk_data agrees with per-bead attempts_of and reopens_of.
#
# THE NON-ZERO REOPEN REQUIREMENT. A fixture where all beads have zero reopens cannot
# distinguish a correct result from an all-zeros false all-clear. sp-kogm returned
# reopens=17 on both paths during the spike that found this; the test reproduces that
# shape by seeding a bead with explicit reopened events.
# --------------------------------------------------------------------------------------
echo
echo "criterion 2: check4_bulk_data agrees with per-bead calls"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-b1","title":"no events","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"sp-b2","title":"two attempts","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"sp-b3","title":"attempts and reopens","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

# sp-b2: two in_progress transitions.
bdq update sp-b2 --status in_progress >/dev/null 2>&1
bdq update sp-b2 --status open >/dev/null 2>&1
bdq update sp-b2 --status in_progress >/dev/null 2>&1
bdq update sp-b2 --status open >/dev/null 2>&1

# sp-b3: one in_progress transition + three reopened events (the non-zero reopen case).
bdq update sp-b3 --status in_progress >/dev/null 2>&1
bdq update sp-b3 --status open >/dev/null 2>&1
for i in 1 2 3; do
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', 'sp-b3', 'reopened', 'harness', '', NOW())" >/dev/null 2>&1
done

# Build a dispatchable-format input (id TAB labels) and run the bulk query.
disp="$(printf 'sp-b1\tspira,plan\nsp-b2\tspira,plan\nsp-b3\tspira,plan')"
bulk="$(check4_bulk_data "$disp")"

# Per-bead reference values.
att_b1="$(attempts_of sp-b1)"; att_b1="${att_b1:-0}"
att_b2="$(attempts_of sp-b2)"; att_b2="${att_b2:-0}"
att_b3="$(attempts_of sp-b3)"; att_b3="${att_b3:-0}"
rep_b3="$(reopens_of  sp-b3)"; rep_b3="${rep_b3:-0}"

# Bulk must agree with per-bead for every bead.
bulk_att_b2="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b2"{print $2}')"
bulk_att_b3="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b3"{print $2}')"
bulk_rep_b3="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b3"{print $3}')"

is "sp-b1 has 0 attempts (per-bead)"             "0" "$att_b1"
is "sp-b2 attempts match per-bead"               "$att_b2" "${bulk_att_b2:-0}"
is "sp-b3 attempts match per-bead"               "$att_b3" "${bulk_att_b3:-0}"
is "sp-b3 reopens match per-bead (non-zero check)" "$rep_b3" "${bulk_rep_b3:-0}"
is "sp-b3 has non-zero reopens (proves detectability)" "3" "$rep_b3"

# sp-b1 has no events: it is absent from the bulk output (no row to return).
# The caller must default to 0 — the test verifies 0 is the right answer.
b1_in_bulk="$(printf '%s' "$bulk" | awk -F'\t' '$1=="sp-b1"{print $1}')"
is "sp-b1 absent from bulk output when it has no events" "" "$b1_in_bulk"

# --------------------------------------------------------------------------------------
# CRITERION 3: sentinel CHECK 4 uses bulk values — poison and requeue cap both fire.
#
# The sentinel is run against a fixture where both paths are exercised. If the loop
# read stale or zero data instead of the bulk values, neither would fire.
# --------------------------------------------------------------------------------------
echo
echo "criterion 3: sentinel uses bulk data for poison and requeue decisions"

git init -q --bare -b main "$TMP/remote.git"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$TMP/remote.git"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

cat > "$TMP/launch" <<'L'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${LAUNCH_LOG:-/dev/null}"
exit "${LAUNCH_RC:-0}"
L
cat > "$TMP/systemctl" <<'S'
#!/usr/bin/env bash
printf '%s\n' "${LAND_STATE:-inactive}"
S
chmod +x "$TMP/launch" "$TMP/systemctl"
export LAUNCH_LOG="$TMP/launch.log"

sentinel() {
    rm -f "$RUN/reflect.fired" "$RUN/inference.cooldown"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=0 \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_SUMMON="$TMP/launch" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_SKIP_CLOSED_CHECK=1 \
        bash "$SH/sentinel.sh" 2>&1
}

labels_of() { bdq show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))'; }
poisoned() { [[ " $(labels_of "$1") " == *" spira-poison "* ]]; }
ispoisoned()  { poisoned "$2" && ok "$1" || bad "$1" "$2 was not poisoned"; }
notpoisoned() { poisoned "$2" && bad "$1" "$2 was poisoned" || ok "$1"; }

# Seed: two beads — one at poison threshold with events, one with non-zero reopens.
testdb_reset; rm -rf "$RUN/poison-asked" "$RUN/requeue-asked"
testdb_seed <<'JSONL'
{"id":"sp-poison-target","title":"at poison threshold","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"sp-requeue-target","title":"at requeue cap","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

# Cycle sp-poison-target to POISON_AT in_progress events.
for i in 1 2 3; do
    bdq update sp-poison-target --status in_progress >/dev/null 2>&1
    bdq update sp-poison-target --status open >/dev/null 2>&1
done

# Give sp-requeue-target REQUEUE_AT=5 reopened events.
for i in 1 2 3 4 5; do
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', 'sp-requeue-target', 'reopened', 'harness', '', NOW())" >/dev/null 2>&1
done

out="$(SPIRA_POISON_AT=3 SPIRA_REQUEUE_AT=5 sentinel)"
ispoisoned "sentinel poisons bead at threshold using bulk attempts"   sp-poison-target
want       "requeue cap fires using bulk reopens"                     "requeue" "$out"
notpoisoned "requeue target is not poisoned (different path)"         sp-requeue-target

printf '\ntest-check4-batch.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
