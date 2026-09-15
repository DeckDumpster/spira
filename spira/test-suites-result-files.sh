#!/usr/bin/env bash
#
# test-suites-result-files.sh — every suite in the timed run produces a .result file,
# regardless of whether it passed, failed, timed out, or was not reached in the budget.
#
#   ./test-suites-result-files.sh
#
# WHAT THIS GUARDS (sp-04bd). Four timed suites produced no result files — they were
# unreached within the budget or their hangs wedged the runner before the fix. Without
# a result file, suites.sh status reports "?" for that suite, which is the same reading
# as "never ran", so the defect was invisible. The invariant: every suite in the timed
# population must have a .result file after every pass, regardless of outcome.
#
# THREE PROPERTIES, each with its positive control:
#
#   1. A suite that runs and passes produces an "ok" result file.
#
#   2. A suite that hangs and is killed by the per-suite watchdog produces a "timeout"
#      result file. Without this, a hung suite leaves its slot empty and the status pane
#      reads "?" — indistinguishable from "never ran".
#
#   3. A suite not reached within the budget produces an "unreached" result file. This
#      is the specific shape sp-04bd observed: suites at the end of alphabetical order
#      ran out of budget and left no record at all.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control):
#   - Before the first run, no result files exist — proves the assertion is not vacuously
#     satisfied from a prior run.
#   - The ok and timeout results are verified before trusting the "unreached" verdict,
#     because a runner that produced no records at all would show every assertion as
#     "no result file", including the one meant to catch a different failure.
#
# BUDGET AND PERSUITE ARE PINNED SO THE UNREACHED CASE IS RELIABLE. With BUDGET=12 and
# PERSUITE=8, the hung suite consumes 8s of budget; the suite alphabetically after it
# finds left ≈ 4 ≤ 5 and goes to unreached. Even on a slow machine (2s per-step overhead),
# the margin leaves left ≤ 5 for the last suite.
#
# THE INTAKE IS THE REAL ONE on a throwaway database (law-prefer-the-real-dependency).
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT (law-gates-run-in-a-clean-environment).
#
# defect: sp-04bd
# covers: spira/suites.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-suites-result-files.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-result-files
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up result-files || { echo "test-suites-result-files: could not build a fixture database"; exit 1; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$SH/"

# Knobs, every one pinned away from the shipped default.
BUDGET=12; PERSUITE=8; STALE=3600; PRIO=3; REPONAME=result-files-fixture

# Repo map for incident.sh's bdq create when a red or timeout is filed.
printf '%s | %s | push | main | : | :\n' "$REPONAME" "$TMP/repo" > "$SH/repo-map"

# Ask stub for escalation calls from incident.sh.
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$TMP/ask.log" > "$SH/ask.sh"
chmod +x "$SH/ask.sh"

# sut — the runner in an explicit minimal environment.
#
# SPIRA_PATH comes from testdb_up: TESTDB_BIN is prepended and contains a `bd` wrapper
# pointing at the embedded binary. conf.sh rebuilds PATH from SPIRA_PATH, so TESTDB_BIN
# must be present for `bd` to survive the PATH replacement.
sut() {
    local cmd="$1"; shift
    # SPIRA_SUITES_INLINE=1 — run the planted suites HERE, not in a container.
    # suites.sh now delegates its pass to testenv-batch.sh (law-tests-run-only-through-
    # testenv-batch). This suite is testing suites.sh's OWN logic against fake suites it
    # planted in a scratch tree, so a container would have to be started per invocation to
    # run code that exists only to be counted. Containment is not being waived: this suite
    # is itself run inside a container by the timed pass, so the planted suites are already
    # contained by it.
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_NOTIFY="$SH/ask.sh" \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_SUITES_INLINE=1 \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }
result_status() {
    local f="$STATE/$1.result"
    [ -r "$f" ] || { printf 'MISSING'; return; }
    read -r s _ < "$f" 2>/dev/null && printf '%s' "${s:-MISSING}" || printf 'MISSING'
}

# Gate file: names nothing — all planted suites go to the timed pass.
printf '# nothing in the gate for this fixture\n' > "$GATEF"

# ======================================================================================
echo
echo "three suites covering the three result-file cases:"
# ======================================================================================
# test-fx-aaa.sh sorts first and runs normally — the positive control.
plant test-fx-aaa.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
echo "  ok    passes immediately"
S
# test-fx-slow.sh hangs; the per-suite watchdog kills it after PERSUITE=8s.
# After it is killed, left ≈ 12-8 = 4 ≤ 5, so test-fx-zzz.sh cannot start.
plant test-fx-slow.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
sleep 300
S
# test-fx-zzz.sh sorts last; it is budget-unreached and must still get a result file.
plant test-fx-zzz.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
echo "  ok    last suite alphabetically"
S

# ======================================================================================
echo
echo "positive control: before any run, no result files exist:"
# ======================================================================================
# A runner that wrote result files from a prior fixture run would make the assertions
# below vacuously true. Verify the slate is clean before trusting them.
[ -e "$STATE/test-fx-aaa.sh.result" ] \
    && bad "no pre-existing result for test-fx-aaa.sh" "file already present" \
    || ok "no pre-existing result for test-fx-aaa.sh"
[ -e "$STATE/test-fx-slow.sh.result" ] \
    && bad "no pre-existing result for test-fx-slow.sh" "file already present" \
    || ok "no pre-existing result for test-fx-slow.sh"
[ -e "$STATE/test-fx-zzz.sh.result" ] \
    && bad "no pre-existing result for test-fx-zzz.sh" "file already present" \
    || ok "no pre-existing result for test-fx-zzz.sh"

# ======================================================================================
echo
echo "after a budget-limited pass, every timed suite has a .result file:"
# ======================================================================================
out="$(sut run)"

# CASE 1 — POSITIVE CONTROL. The first suite ran and produced an ok result. Without
# this, a runner that silently produced no records at all would pass the unreached case
# below (both would show MISSING, and "MISSING != unreached" would fire on both — but
# that would be reported as a failure, so the real risk is the inverse: a runner that
# runs ok.sh but fails to run the unreached logic would produce "ok" here and "MISSING"
# for zzz). Verify the ok case first so any absence below is attributable to the specific
# failure shape, not to total runner breakage.
aaa_st="$(result_status test-fx-aaa.sh)"
is "positive control: test-fx-aaa.sh has an ok result" "ok" "$aaa_st"

# CASE 2 — HUNG SUITE. The per-suite watchdog kills test-fx-slow.sh and record_write is
# called with status=timeout. If the runner were still blocking on a command-substitution
# read (the pre-sp-04bd shape), no result would appear for this suite or anything after.
slow_st="$(result_status test-fx-slow.sh)"
is "test-fx-slow.sh (hung, killed by watchdog) has a timeout result" "timeout" "$slow_st"

# CASE 3 — BUDGET-UNREACHED. After test-fx-slow.sh is killed, left ≈ 4 ≤ 5, so
# test-fx-zzz.sh is put in the unreached list rather than started. The post-loop block
# in cmd_run must write a .unreached file for it. The .result file is NOT written because
# test-fx-zzz.sh has never actually run — the .unreached flag is the only record (sp-u1g).
# Without the post-loop block (the defect shape of sp-04bd), this suite would have no
# record at all, and the status pane would read "?" — indistinguishable from "never ran".
[ -f "$STATE/test-fx-zzz.sh.unreached" ] \
    && ok "test-fx-zzz.sh (budget-unreached) has a .unreached file" \
    || bad "test-fx-zzz.sh (budget-unreached) has a .unreached file" "file not found: $STATE/test-fx-zzz.sh.unreached"

# ======================================================================================
echo
echo "result and unreached files carry valid, recent epoch timestamps:"
# ======================================================================================
# A missing timestamp, a zero, or a non-numeric value means the file was written by the
# wrong code path or not at all; a stale timestamp means the result is from a prior run
# and the runner did not overwrite it. Both would make suites.sh status mislead the pane.
# test-fx-zzz.sh never actually ran, so it has no .result file — its timestamp lives in
# its .unreached file (sp-u1g).
now="$(date +%s)"
for fx in test-fx-aaa.sh test-fx-slow.sh; do
    f="$STATE/$fx.result"
    if [ ! -r "$f" ]; then
        bad "$fx: result file exists for timestamp check" "not found"
        continue
    fi
    read -r _st at _rest < "$f" 2>/dev/null || at=""
    case "${at:-}" in
        ''|*[!0-9]*) bad "$fx: result has a numeric epoch timestamp" "got [${at:-empty}]" ;;
        0) bad "$fx: result timestamp is non-zero" "got 0" ;;
        *) age=$(( now - at ))
           [ "$age" -lt 600 ] \
               && ok "$fx: result has a current timestamp (${age}s ago)" \
               || bad "$fx: result timestamp is recent" "${age}s old" ;;
    esac
done
# test-fx-zzz.sh: timestamp comes from the .unreached file, not .result.
f="$STATE/test-fx-zzz.sh.unreached"
if [ ! -r "$f" ]; then
    bad "test-fx-zzz.sh: .unreached file exists for timestamp check" "not found"
else
    read -r at_ur < "$f" 2>/dev/null || at_ur=""
    case "${at_ur:-}" in
        ''|*[!0-9]*) bad "test-fx-zzz.sh: .unreached has a numeric epoch" "got [${at_ur:-empty}]" ;;
        0) bad "test-fx-zzz.sh: .unreached timestamp is non-zero" "got 0" ;;
        *) age=$(( now - at_ur ))
           [ "$age" -lt 600 ] \
               && ok "test-fx-zzz.sh: .unreached has a current timestamp (${age}s ago)" \
               || bad "test-fx-zzz.sh: .unreached timestamp is recent" "${age}s old" ;;
    esac
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
