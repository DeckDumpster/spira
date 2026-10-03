#!/usr/bin/env bash
#
# test-czar-pass.sh — czar fast pass: budget, flock, and acceptance detectors
#
# WHAT THIS SUITE CHECKS.
#   0. cargo test -p czar-pass: every detect_* function's fire/silent boundary against a
#      scratch Config, run as Rust #[test]s rather than through the binary.
#   1. czar-pass is found on PATH and accepts --pass.
#   2. Pass with an empty environment completes in under 5 s (budget).
#   3. A concurrent pass is skipped (flock prevents overlap).
#   4. ci-stalled acceptance case: fixture with a queued CI job > threshold →
#      DETECTED=yes in czar.log; must FAIL against the previous code (no czar.sh).
#   6. Shadow mode: CZAR-WOULD written to czar.log; no bead filed.
#   7. Act mode: deterministic remedy executed; inference bead filed.
#   8. World halted: pass exits 0 without acting.
#   9. SPIRA_LOOP_STALL_SECS and SPIRA_CI_RED_MAX_SECS are in conf.sh allowlist.
#  10. spira-czar-pass.timer and .service exist in systemd/.
#  11. watchtower --queue-checks is deleted (not even a stub; sp-lnmbq).
#  12. sentinel no longer calls watchtower --queue-checks.
#  16-21. base-red: the base ref's OWN gate run, not a batch's. Positive control
#      (green base → no bead), red base → P0+express bead naming the failing suites
#      (replaying the 2026-09-24 shape: test-install-bootstrap-release.sh and
#      test-watch-refresh.sh), a same-red second pass computing the same dedupe ref,
#      and an unreadable base — grace-suppressed at first, then filed as unreadable
#      (never treated as green) once it outlasts the grace window.
#  22-25. deadlock/attribution-failed/sort-failed/loop-stalled: the four log-pattern
#      detectors, ported from test-watchtower-queue.sh (UC-23) — this suite is now the
#      one place that exercises them against the real binary.
#  26. marker advances after each pass — the same landing.log line is not re-detected.
#  27. ci-stalled: the dedupe ref is stable across passes even when the measured duration
#      changes (ported from test-watchtower-queue.sh).
#  28-30. attribution-failed/loop-stalled/ci-stalled: each detector's own trigger-condition
#      boundary (requeued=0 no-op, multi-digit requeued, missing/silent log, configurable
#      threshold, below-threshold, malformed open file) — ported from test-watchtower-queue.sh,
#      which is retired once these land (UC-23).
#  32. reconciler engine wiring: a failed forge call reports STATUS=unobservable, never
#      STATUS=satisfied — the old code could not distinguish that from no CI activity.
#  33. reconciler engine wiring: a deterministic remedy that does not close its gap by
#      the next pass escalates instead of being retried blind.
#  34. pool-idle (sp-forah): the certified-pool trigger's harness-owned replacement for a
#      chat-session while-true loop / re-armed Monitor (sp-ji62y). Positive controls first
#      (pool below SPIRA_QUEUE_ROUND_MIN_N stays quiet; a batch already open stays quiet
#      however full the pool — deliberate back-pressure), then SEEN RED: a full pool with no
#      batch open, past SPIRA_QUEUE_ROUND_STALL_SECS, fires and files an incident naming the
#      repo and depth — nothing alarms on this today without czar.sh's pool-idle detector.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): for detector 4, the test
# first verifies NO detection with an empty/fresh fixture, then adds the trigger and
# verifies detection. A detector that fires on empty data is not a detector.
#
# tier: T1
# covers: czar-pass/src/main.rs reconciler-engine/src/**.rs spira/conf.sh sentinel/src/* watchtower/src/* systemd/spira-czar-pass.service systemd/spira-czar-pass.timer UC-ops-detection-remediation-23 UC-ops-detection-remediation-24
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }

# sp-8fsql: czar.sh (a one-line `exec czar-pass "$@"` shim) is retired; every caller invokes
# czar-pass directly by bare name on the release PATH (sp-gypjk), the same as forge before it.
CZAR="$(command -v czar-pass 2>/dev/null || true)"
[ -n "$CZAR" ] && [ -x "$CZAR" ] || { printf 'czar-pass not found on PATH\n' >&2; exit 2; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# Find or build the czar-pass binary (law-absence-needs-a-positive-control).
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-czar-pass: cargo not found — czar-pass binary cannot be built"
    exit 77
fi
CZAR_PASS_ROOT="$HERE/../czar-pass"

# ==========================================================================================
printf '\n%s\n' "0. cargo test -p czar-pass: every detector's fire/silent boundary (UC-ops-detection-remediation-23)"
# ==========================================================================================
# A separate, scratch CARGO_TARGET_DIR: this must never share the release build below (a
# debug-profile test build and a release build of the same crate under one target dir just
# means two full compiles instead of one, not a correctness problem, but there is no reason
# to pay for the first one twice across runs).
CARGO_TEST_LOG="$T/cargo-test-czar-pass.log"
if CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/czar-pass-test-target" \
    "$CARGO_BIN" test --manifest-path "$CZAR_PASS_ROOT/Cargo.toml" -p czar-pass \
    >"$CARGO_TEST_LOG" 2>&1
then
    ok "cargo test -p czar-pass ($(grep -c '^test ' "$CARGO_TEST_LOG" 2>/dev/null || echo ?) tests)"
else
    bad "cargo test -p czar-pass (see $CARGO_TEST_LOG)"
    tail -60 "$CARGO_TEST_LOG" >&2
fi

# czar-pass is found directly on this suite's PATH (sp-gypjk).

# Minimal test environment — no real database needed: incident is stubbed,
# summon_fayth silently returns 1 when fayth_ready finds no db (|| true guards it).
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_DB="$T/db"
mkdir -p "$SPIRA_RUN/queue" "$SPIRA_DB"

# Stub forge.sh: returns empty by default (no CI activity)
STUB_FORGE="$T/forge.sh"
cat > "$STUB_FORGE" <<'FEOF'
#!/usr/bin/env bash
cmd="${1:-}"
case "$cmd" in
    batch-ci-status|queued-since|run-id) exit 0 ;;
    workflow-rerun) printf 'stub: workflow-rerun %s\n' "${3:-}" >> "$FORGE_LOG"; exit 0 ;;
    *) exit 0 ;;
esac
FEOF
chmod +x "$STUB_FORGE"
export SPIRA_FORGE="$STUB_FORGE"
export FORGE_LOG="$T/forge-calls.log"

# Stub incident.sh: records cause and ref, exits 0
STUB_INC="$T/incident.sh"
cat > "$STUB_INC" <<'IEOF'
#!/usr/bin/env bash
printf 'incident: cause=%s ref=%s subj=%s priority=%s labels=%s\n' \
    "${SPIRA_INCIDENT_CAUSE:-}" "${SPIRA_INCIDENT_REF:-}" "${2:-}" \
    "${SPIRA_INCIDENT_PRIORITY:-}" "${SPIRA_INCIDENT_LABELS:-}" >> "$INC_LOG"
exit 0
IEOF
chmod +x "$STUB_INC"
export SPIRA_INCIDENT_SH="$STUB_INC"
export INC_LOG="$T/incident-calls.log"

# Never actually summon an aeon
export SPIRA_SUMMON=true

printf 'test-czar-pass.sh\n'

# ==========================================================================================
printf '\n%s\n' "1. czar-pass exists on PATH and accepts --pass"
# ==========================================================================================
[ -f "$CZAR" ] && ok "czar-pass exists" || bad "czar-pass not found at $CZAR"
[ -x "$CZAR" ] && ok "czar-pass is executable" || bad "czar-pass not executable"

# ==========================================================================================
printf '\n%s\n' "2. budget: empty environment completes in under 5s"
# ==========================================================================================
_t0="$(date +%s)"
"$CZAR" --pass >/dev/null 2>&1
_t1="$(date +%s)"
_elapsed=$(( _t1 - _t0 ))
[ "$_elapsed" -lt 5 ] \
    && ok "pass completed in ${_elapsed}s (budget: 5s)" \
    || bad "pass exceeded budget: ${_elapsed}s (limit: 5s)"

# ==========================================================================================
printf '\n%s\n' "3. flock: concurrent pass is skipped"
# ==========================================================================================
# Hold the lock manually, then try to run the pass; it must exit 0 immediately.
_lock="$SPIRA_RUN/czar-pass.lock"
exec 9>"$_lock"; flock 9
_flock_out="$("$CZAR" --pass 2>&1 || true)"
exec 9>&-
want "concurrent pass logs 'already running'" "already running" "$_flock_out"

# ==========================================================================================
printf '\n%s\n' "4. ci-stalled acceptance case (POSITIVE CONTROL first)"
# ==========================================================================================
# POSITIVE CONTROL: no open batch → ci-stalled not detected.
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-stalled: DETECTED=no when no open batch" "CLASS=ci-stalled DETECTED=no" "$_log"

# SEEN RED: add a fixture open batch whose CI job has been queued for > threshold.
_now_e="$(date +%s)"
_old_e=$(( _now_e - 700 ))   # 700s > default threshold of 600s
_batch_dir="$SPIRA_RUN/queue/testrepo"
mkdir -p "$_batch_dir"
printf 'branch=spira/queue/test\n' > "$_batch_dir/open"

# Forge stub now returns queued-since for batch-ci-status
cat > "$STUB_FORGE" <<FEOF2
#!/usr/bin/env bash
cmd="\${1:-}"
case "\$cmd" in
    batch-ci-status)
        printf 'run-id: 12345\n'
        printf 'queued-since: ${_old_e}\n'
        ;;
    workflow-rerun) printf 'stub: workflow-rerun %s\n' "\${3:-}" >> "\$FORGE_LOG" ;;
esac
exit 0
FEOF2
chmod +x "$STUB_FORGE"

# Register testrepo in the repo map (pipe-separated: name | path | land | ...)
export SPIRA_REPO_MAP="$T/repo-map"
mkdir -p "$T/testrepo"
printf 'testrepo | %s | push | origin/main | |\n' "$T/testrepo" > "$SPIRA_REPO_MAP"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."*
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-stalled: DETECTED=yes in czar.log" "CLASS=ci-stalled DETECTED=yes" "$_log"

# ==========================================================================================
printf '\n%s\n' "6. shadow mode: CZAR-WOULD written, no bead filed"
# ==========================================================================================
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* \
      "$INC_LOG"
printf '%s spira: verdict spira: PR 90 red — no suites identified; leaving batch open\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "shadow: CZAR-WOULD written to czar.log for deadlock" "CZAR-WOULD: deadlock" "$_log"
lack "shadow: no incident filed in shadow mode" "incident: cause=deadlock" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "7. act mode: inference bead filed for deadlock"
# ==========================================================================================
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* \
      "$INC_LOG"
printf '%s spira: verdict spira: PR 91 red — no suites identified; leaving batch open\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
SPIRA_CZAR_STAGE_DEADLOCK=act "$CZAR" --pass >/dev/null 2>&1
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "act: incident filed with cause=deadlock" "cause=deadlock" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "8. world halted: pass exits without acting"
# ==========================================================================================
touch "$SPIRA_RUN/world.halted"
rm -f "$SPIRA_RUN/czar.log" "$INC_LOG"
_halt_out="$("$CZAR" --pass 2>&1)"
_rc=$?
rm -f "$SPIRA_RUN/world.halted"
[ "$_rc" -eq 0 ] && ok "halted: exits 0" || bad "halted: expected exit 0, got $_rc"
want "halted: logs 'world is halted'" "world is halted" "$_halt_out"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
lack "halted: no incident filed" "incident:" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "9. new config keys in conf.sh allowlist"
# ==========================================================================================
conf_sh="$HERE/conf.sh"
# sp-g3uwp: conf.sh no longer carries its allowlist as literal text — a key's membership
# is now the existence of its own file under conf.d/, which conf-gen.sh derives the
# allowlist from directly.
for key in SPIRA_LOOP_STALL_SECS SPIRA_CI_RED_MAX_SECS; do
    [ -f "$HERE/conf.d/$key" ] \
        && ok "$key in SPIRA_CONF_KEYS" \
        || bad "$key missing from conf.sh"
done

# ==========================================================================================
printf '\n%s\n' "10. systemd unit files exist"
# ==========================================================================================
_sd="$HERE/../systemd"
[ -f "$_sd/spira-czar-pass.timer" ] \
    && ok "spira-czar-pass.timer exists" \
    || bad "spira-czar-pass.timer not found"
[ -f "$_sd/spira-czar-pass.service" ] \
    && ok "spira-czar-pass.service exists" \
    || bad "spira-czar-pass.service not found"
_timer="$(cat "$_sd/spira-czar-pass.timer" 2>/dev/null)"
want "timer: OnUnitActiveSec=30s" "OnUnitActiveSec=30s" "$_timer"

# ==========================================================================================
printf '\n%s\n' "11. watchtower --queue-checks is deleted, not retired (sp-lnmbq)"
# ==========================================================================================
# watchtower.sh's retirement stub (the six queue-stall detectors moved here, sp-rpibz) is
# gone along with the rest of the bash: the Rust watchtower crate never had a --queue-checks
# handler at all, retire-rather-than-port (DESIGN.md §4). It is simply not a recognized
# argument now.
_wt_rc=0
_wt_out="$(SPIRA_RUN="$SPIRA_RUN" watchtower --queue-checks 2>&1)" || _wt_rc=$?
is "watchtower: --queue-checks is not a recognized argument" "2" "$_wt_rc"
# The queue-stall detector logic must not be in the watchtower crate either.
lack "watchtower: no 'ejected 0' detector in the watchtower crate" "ejected 0, requeued" \
    "$(cat "$HERE"/../watchtower/src/*.rs "$HERE"/../watchtower/src/sweep/*.rs 2>/dev/null)"

# ==========================================================================================
printf '\n%s\n' "12. the sentinel does not call watchtower --queue-checks"
# ==========================================================================================
lack "sentinel: no watchtower --queue-checks call" \
    "--queue-checks" "$(cat "$HERE"/../sentinel/src/*.rs)"

# ==========================================================================================
printf '\n%s\n' "13. ci-red: batch old, run just turned red → DETECTED=no (POSITIVE CONTROL)"
# ==========================================================================================
# The batch opened 30 min ago; the CI run completed (failure) only 60s ago.
# The detector must NOT fire: the verdict has not had its turn yet.
_now_e2="$(date +%s)"
_batch_dir2="$SPIRA_RUN/queue/redrepo"
mkdir -p "$_batch_dir2"
printf 'branch=spira/queue/test-red\n' > "$_batch_dir2/open"
touch -d "30 minutes ago" "$_batch_dir2/open"

_ci_completed_recent=$(( _now_e2 - 60 ))   # completed 60s ago
cat > "$STUB_FORGE" <<FEOF13
#!/usr/bin/env bash
cmd="\${1:-}"
case "\$cmd" in
    batch-ci-status)
        printf 'run-id: 99999\n'
        printf 'run-conclusion: failure\n'
        printf 'run-completed-at: %s\n' "${_ci_completed_recent}"
        ;;
esac
exit 0
FEOF13
chmod +x "$STUB_FORGE"

export SPIRA_REPO_MAP="$T/repo-map"
mkdir -p "$T/redrepo"
printf 'redrepo | %s | push | origin/main | |\n' "$T/redrepo" >> "$SPIRA_REPO_MAP"

export SPIRA_CI_RED_MAX_SECS=600
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."*
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-red: DETECTED=no when run just turned red (60s < 600s threshold)" \
    "CLASS=ci-red DETECTED=no" "$_log"

# ==========================================================================================
printf '\n%s\n' "14. ci-red: run red for >threshold → DETECTED=yes, interval from completion"
# ==========================================================================================
# The batch opened 30 min ago; the CI run completed (failure) 700s ago.
# The detector must fire. The logged interval must be ~700s (not ~1800s from batch age).
_ci_completed_old=$(( _now_e2 - 700 ))   # completed 700s ago; 700 > 600 threshold
cat > "$STUB_FORGE" <<FEOF14
#!/usr/bin/env bash
cmd="\${1:-}"
case "\$cmd" in
    batch-ci-status)
        printf 'run-id: 99999\n'
        printf 'run-conclusion: failure\n'
        printf 'run-completed-at: %s\n' "${_ci_completed_old}"
        ;;
esac
exit 0
FEOF14
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."*
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-red: DETECTED=yes when run red >threshold" "CLASS=ci-red DETECTED=yes" "$_log"
lack "ci-red: interval is not ~1800s (batch-open age)" "not acting for 1" "$_log"
want "ci-red: czar.log shows 'not acting for 7'" "not acting for 7" "$_log"

# ==========================================================================================
printf '\n%s\n' "15. ci-stalled unaffected by ci-red fix (positive control for coexistence)"
# ==========================================================================================
# Same pass: ci job queued > threshold (ci-stalled fires) + run just turned red (ci-red quiet).
# Uses a single repo that shows both conditions simultaneously.
_ci_queued_old=$(( _now_e2 - 700 ))
cat > "$STUB_FORGE" <<FEOF15
#!/usr/bin/env bash
cmd="\${1:-}"
case "\$cmd" in
    batch-ci-status)
        printf 'run-id: 88888\n'
        printf 'queued-since: %s\n' "${_ci_queued_old}"
        printf 'run-completed-at: %s\n' "$(( _now_e2 - 60 ))"
        ;;
    workflow-rerun) printf 'stub: workflow-rerun\n' >> "\$FORGE_LOG" ;;
esac
exit 0
FEOF15
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."*
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "coexistence: ci-stalled still DETECTED=yes" "CLASS=ci-stalled DETECTED=yes" "$_log"
want "coexistence: ci-red stays DETECTED=no (run only 60s red)" "CLASS=ci-red DETECTED=no" "$_log"

# ==========================================================================================
printf '\n%s\n' "16. base-red: green base → DETECTED=no (POSITIVE CONTROL)"
# ==========================================================================================
# Open batch on its own branch; the stub answers batch-ci-status only for branch=main
# (the repo's base, from repo-map column 4) so ci-stalled/ci-red on the batch's own
# branch stay silent and this section isolates base-red alone.
_now_e3="$(date +%s)"
_br_dir="$SPIRA_RUN/queue/baseredrepo"
mkdir -p "$_br_dir"
printf 'branch=spira/queue/baseredtest\n' > "$_br_dir/open"
mkdir -p "$T/baseredrepo"
printf 'baseredrepo | %s | queue | origin/main | |\n' "$T/baseredrepo" >> "$SPIRA_REPO_MAP"

cat > "$STUB_FORGE" <<'FEOF16'
#!/usr/bin/env bash
cmd="${1:-}"; branch_arg="${3:-}"
case "$cmd" in
    batch-ci-status)
        if [ "$branch_arg" = "main" ]; then
            printf 'run-id: 55555\n'
            printf 'run-conclusion: success\n'
            printf 'head-sha: deadbeef\n'
        fi
        ;;
esac
exit 0
FEOF16
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "base-red: DETECTED=no when base's own run is green" "CLASS=base-red DETECTED=no" "$_log"
lack "base-red: no bead filed for a green base" "cause=base-red" "$(cat "$INC_LOG" 2>/dev/null || true)"

# ==========================================================================================
printf '\n%s\n' "17. base-red: red base → P0+express bead naming the failing suites"
# ==========================================================================================
# Replays the 2026-09-24 shape: a batch lands, the base's own full-corpus run comes
# back red on suites the batch's own diff-selected run never touched.
cat > "$STUB_FORGE" <<'FEOF17'
#!/usr/bin/env bash
cmd="${1:-}"; branch_arg="${3:-}"
case "$cmd" in
    batch-ci-status)
        if [ "$branch_arg" = "main" ]; then
            printf 'run-id: 55556\n'
            printf 'run-conclusion: failure\n'
            printf 'head-sha: cafef00d\n'
            printf 'run-url: https://example.invalid/actions/runs/55556\n'
            printf 'red-suite: test-watch-refresh.sh\n'
            printf 'red-suite: test-install-bootstrap-release.sh\n'
        fi
        ;;
esac
exit 0
FEOF17
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
SPIRA_CZAR_STAGE_BASE_RED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "base-red: DETECTED=yes when base's own run is red" "CLASS=base-red DETECTED=yes" "$_log"
want "base-red: bead filed with cause=base-red" "cause=base-red" "$_inc_log"
want "base-red: filed at priority 0 (P0)" "priority=0" "$_inc_log"
want "base-red: filed with the express label" "labels=" "$_inc_log"
want "base-red: express label present in filed labels" "express" "$_inc_log"
want "base-red: subject names a failing suite" "test-watch-refresh.sh" "$_inc_log"
want "base-red: subject names the other failing suite" "test-install-bootstrap-release.sh" "$_inc_log"
_ref17="$(printf '%s\n' "$_inc_log" | grep -o 'ref=[^ ]*' | tail -1)"

# ==========================================================================================
printf '\n%s\n' "18. base-red: still red on a second pass → same dedupe ref (recurrence, not a new bead)"
# ==========================================================================================
# incident.sh itself owns dedupe-on-external_ref (tested in test-incident.sh); this
# proves czar-pass computes the SAME ref on repeated passes for an unchanged suite set,
# which is the half of the dedupe contract that lives in this seam.
rm -f "$SPIRA_RUN/czar-pass.swept" "$INC_LOG"
SPIRA_CZAR_STAGE_BASE_RED=act "$CZAR" --pass >/dev/null 2>&1
_ref18="$(grep -o 'ref=[^ ]*' "$INC_LOG" 2>/dev/null | tail -1)"
is "base-red: dedupe ref is stable across passes with the same suite set" "$_ref17" "$_ref18"

# ==========================================================================================
printf '\n%s\n' "19. base-red: unreadable status, still inside grace → DETECTED=no (POSITIVE CONTROL)"
# ==========================================================================================
# The forge seam returns nothing for the base branch at all (no run-id) — the same shape
# as a genuine API failure OR a push whose run GitHub has not created yet. A fresh
# first-seen marker (age 0) must not page: that would fire on every ordinary landing.
cat > "$STUB_FORGE" <<'FEOF19'
#!/usr/bin/env bash
exit 0
FEOF19
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "base-red: DETECTED=no while unreadable is still inside grace" \
    "CLASS=base-red DETECTED=no" "$_log"
lack "base-red: no bead filed while still inside grace" "cause=base-red" \
    "$(cat "$INC_LOG" 2>/dev/null || true)"

# ==========================================================================================
printf '\n%s\n' "20. base-red: unreadable past grace → filed as unreadable, not treated as green"
# ==========================================================================================
_old_unreadable=$(( _now_e3 - 200 ))   # 200s > default 120s grace
# The reconciler's hysteresis lives in one persisted state file, keyed per invariant
# (sp-pu7v6) — seeding "since" 200s in the past replays what a marker file used to do.
python3 -c "
import json
st = {'base-red:baseredrepo': {'since': $_old_unreadable, 'remedy_attempted_at': None, 'remedy_desc': None}}
print(json.dumps(st))
" > "$SPIRA_RUN/reconciler-state.json"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG"
SPIRA_CZAR_STAGE_BASE_RED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "base-red: DETECTED=yes once unreadable outlasts grace" "CLASS=base-red DETECTED=yes" "$_log"
want "base-red: bead says status could not be read" "could not be read" "$_inc_log"
want "base-red: unreadable bead still filed at P0" "priority=0" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "21. SPIRA_BASE_CI_UNREADABLE_GRACE_SECS and SPIRA_CZAR_STAGE_BASE_RED in conf.sh allowlist"
# ==========================================================================================
for key in SPIRA_BASE_CI_UNREADABLE_GRACE_SECS SPIRA_CZAR_STAGE_BASE_RED; do
    [ -f "$HERE/conf.d/$key" ] \
        && ok "$key in SPIRA_CONF_KEYS" \
        || bad "$key missing from conf.sh"
done

# ==========================================================================================
printf '\n%s\n' "22. DEADLOCK: no suites identified; leaving batch open"
# ==========================================================================================
# POSITIVE CONTROL first: an unrelated log line must not fire the detector.
cat > "$STUB_FORGE" <<'FEOF22'
#!/usr/bin/env bash
exit 0
FEOF22
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
printf '%s spira: verdict spira: PR 72 red — suites identified; failing attribution\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "deadlock: DETECTED=no when no 'no suites' line" "CLASS=deadlock DETECTED=no" "$_log"

# SEEN RED: the fixture line fires the detector and files an incident in act mode.
printf '%s spira: verdict spira: PR 72 red — no suites identified; leaving batch open\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$SPIRA_RUN/landing.log"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
SPIRA_CZAR_STAGE_DEADLOCK=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "deadlock: DETECTED=yes on 'no suites identified' fixture" "CLASS=deadlock DETECTED=yes" "$_log"
want "deadlock: incident filed with cause=deadlock" "cause=deadlock" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "23. ATTRIBUTION-FAILED: ejected 0, requeued N"
# ==========================================================================================
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
printf '%s spira: verdict spira: PR 73 — ejected 2, requeued 1\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "attribution-failed: DETECTED=no when ejected is non-zero" \
    "CLASS=attribution-failed DETECTED=no" "$_log"

printf '%s spira: verdict spira: PR 73 — ejected 0, requeued 5\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$SPIRA_RUN/landing.log"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "attribution-failed: DETECTED=yes on ejected 0, requeued 5" \
    "CLASS=attribution-failed DETECTED=yes" "$_log"
want "attribution-failed: incident filed with cause=attribution-failed" \
    "cause=attribution-failed" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "24. SORT-FAILED: queue_sort_rows ranking failed"
# ==========================================================================================
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
printf '%s spira: verdict spira: PR 74 red\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "sort-failed: DETECTED=no when ranking did not fail" "CLASS=sort-failed DETECTED=no" "$_log"

printf '%s spira: queue_sort_rows: ranking failed (rc=1) -- returning rows unranked\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$SPIRA_RUN/landing.log"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
SPIRA_CZAR_STAGE_SORT_FAILED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "sort-failed: DETECTED=yes on ranking-failed fixture" "CLASS=sort-failed DETECTED=yes" "$_log"
want "sort-failed: incident filed with cause=sort-failed" "cause=sort-failed" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "25. LOOP-STALLED: no landing pass complete within the threshold"
# ==========================================================================================
# Stub systemctl so is-failed is deterministic: non-zero → inference branch, not det-restart.
STUB_SC="$T/stub-systemctl.sh"
printf '#!/usr/bin/env bash\nexit 1\n' > "$STUB_SC"
chmod +x "$STUB_SC"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
_recent_ts="$(date -u -d '@'"$(( $(date +%s) - 100 ))" +%Y-%m-%dT%H:%M:%SZ)"
printf '%s spira: landing: pass complete — 3 branch(es) seen, 0 movement(s)\n' \
    "$_recent_ts" > "$SPIRA_RUN/landing.log"
SPIRA_SYSTEMCTL="$STUB_SC" "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "loop-stalled: DETECTED=no when last pass is 100s old (threshold 3000s)" \
    "CLASS=loop-stalled DETECTED=no" "$_log"

_old_ts="$(date -u -d '@'"$(( $(date +%s) - 4000 ))" +%Y-%m-%dT%H:%M:%SZ)"
printf '%s spira: landing: pass complete — 3 branch(es) seen, 0 movement(s)\n' \
    "$_old_ts" > "$SPIRA_RUN/landing.log"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
SPIRA_SYSTEMCTL="$STUB_SC" SPIRA_CZAR_STAGE_LOOP_STALLED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "loop-stalled: DETECTED=yes when last pass is 4000s old (threshold 3000s)" \
    "CLASS=loop-stalled DETECTED=yes" "$_log"
want "loop-stalled: incident filed with cause=loop-stalled" "cause=loop-stalled" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "26. marker advances after each pass — same landing.log line not re-detected"
# ==========================================================================================
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
printf '%s spira: verdict spira: PR 72 red — no suites identified; leaving batch open\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
SPIRA_CZAR_STAGE_DEADLOCK=act "$CZAR" --pass >/dev/null 2>&1
_log1="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "marker: first pass detects the line" "CLASS=deadlock DETECTED=yes" "$_log1"

rm -f "$SPIRA_RUN/czar.log"
SPIRA_CZAR_STAGE_DEADLOCK=act "$CZAR" --pass >/dev/null 2>&1
_log2="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
lack "marker: second pass does not re-detect the same line" "CLASS=deadlock DETECTED=yes" "$_log2"

# ==========================================================================================
printf '\n%s\n' "27. ci-stalled: dedupe ref stable across passes with different measured durations"
# ==========================================================================================
# format!("ci-stalled-{}", repo) must depend only on the identifier, never on the measured
# duration — otherwise every pass files a fresh bead instead of bumping incident.sh's
# recurrence count. Same proof as row 18 (base-red).
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
mkdir -p "$SPIRA_RUN/queue/refstable"
printf 'branch=spira/queue/refstable-test\n' > "$SPIRA_RUN/queue/refstable/open"
mkdir -p "$T/refstable"
printf 'refstable | %s | push | origin/main | |\n' "$T/refstable" >> "$SPIRA_REPO_MAP"

# Other open batches from earlier sections (testrepo, redrepo, baseredrepo) are still on
# disk — SPIRA_RUN is never wiped between sections — so the stub must answer only for
# refstable's own branch, never for whichever repo happens to iterate last.
_qs1=$(( $(date +%s) - 700 ))
cat > "$STUB_FORGE" <<FEOF27A
#!/usr/bin/env bash
cmd="\${1:-}"; branch_arg="\${3:-}"
case "\$cmd:\$branch_arg" in
    batch-ci-status:*refstable*) printf 'queued-since: ${_qs1}\n' ;;
esac
exit 0
FEOF27A
chmod +x "$STUB_FORGE"
SPIRA_CZAR_STAGE_CI_STALLED=act "$CZAR" --pass >/dev/null 2>&1
_ref_a="$(grep -o 'ref=[^ ]*' "$INC_LOG" 2>/dev/null | tail -1)"

rm -f "$SPIRA_RUN/czar-pass.swept" "$INC_LOG"
_qs2=$(( $(date +%s) - 5000 ))
cat > "$STUB_FORGE" <<FEOF27B
#!/usr/bin/env bash
cmd="\${1:-}"; branch_arg="\${3:-}"
case "\$cmd:\$branch_arg" in
    batch-ci-status:*refstable*) printf 'queued-since: ${_qs2}\n' ;;
esac
exit 0
FEOF27B
chmod +x "$STUB_FORGE"
SPIRA_CZAR_STAGE_CI_STALLED=act "$CZAR" --pass >/dev/null 2>&1
_ref_b="$(grep -o 'ref=[^ ]*' "$INC_LOG" 2>/dev/null | tail -1)"
is "ci-stalled: dedupe ref stable across passes with different queued durations" \
    "$_ref_a" "$_ref_b"
want "ci-stalled ref names the repo" "refstable" "$_ref_a"

# ==========================================================================================
printf '\n%s\n' "28. ATTRIBUTION-FAILED: requeued 0 is a no-op; multi-digit requeued still fires"
# ==========================================================================================
# Ported from test-watchtower-queue.sh (UC-23): requeued 0 means nothing was actually
# requeued (no-op, not a failure to attribute), and the pattern must not be anchored at a
# single-digit boundary.
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* "$INC_LOG"
printf '%s spira: verdict spira: PR 75 — ejected 0, requeued 0\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "attribution-failed: DETECTED=no when requeued is 0 (no-op)" \
    "CLASS=attribution-failed DETECTED=no" "$_log"

printf '%s spira: verdict spira: PR 76 — ejected 0, requeued 10\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$SPIRA_RUN/landing.log"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "attribution-failed: multi-digit requeued count (10) still fires" \
    "CLASS=attribution-failed DETECTED=yes" "$_log"

# ==========================================================================================
printf '\n%s\n' "29. LOOP-STALLED: no log at all, a log with no pass-complete line, configurable threshold"
# ==========================================================================================
# Ported from test-watchtower-queue.sh (UC-23): a missing or silent landing.log cannot tell
# stalled from never-started, so it must not page — and the threshold read from
# SPIRA_LOOP_STALL_SECS must actually change the outcome, not just exist in the allowlist.
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* \
      "$INC_LOG" "$SPIRA_RUN/landing.log"
SPIRA_SYSTEMCTL="$STUB_SC" "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "loop-stalled: DETECTED=no when landing.log does not exist" \
    "CLASS=loop-stalled DETECTED=no" "$_log"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
printf '%s spira: verdict spira: PR 72 red\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SPIRA_RUN/landing.log"
SPIRA_SYSTEMCTL="$STUB_SC" "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "loop-stalled: DETECTED=no when the log has no 'pass complete' line" \
    "CLASS=loop-stalled DETECTED=no" "$_log"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
_thresh_ts="$(date -u -d '@'"$(( $(date +%s) - 100 ))" +%Y-%m-%dT%H:%M:%SZ)"
printf '%s spira: landing: pass complete — 3 branch(es) seen, 0 movement(s)\n' \
    "$_thresh_ts" > "$SPIRA_RUN/landing.log"
SPIRA_SYSTEMCTL="$STUB_SC" SPIRA_LOOP_STALL_SECS=50 "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "loop-stalled: configurable threshold — 100s age fires at threshold=50s" \
    "CLASS=loop-stalled DETECTED=yes" "$_log"

# ==========================================================================================
printf '\n%s\n' "30. CI-STALLED: below threshold, forge returns nothing queued, malformed open file"
# ==========================================================================================
# Ported from test-watchtower-queue.sh (UC-23): none of these are the shared marker/ref
# mechanism (row 27) — each is ci-stalled's own trigger-condition boundary.
_ci30_dir="$SPIRA_RUN/queue/ci30repo"
mkdir -p "$_ci30_dir"
printf 'branch=spira/queue/ci30-test\n' > "$_ci30_dir/open"
mkdir -p "$T/ci30repo"
printf 'ci30repo | %s | push | origin/main | |\n' "$T/ci30repo" >> "$SPIRA_REPO_MAP"

# Other open batches from earlier sections are still on disk — SPIRA_RUN is never wiped
# between sections — so every stub below answers only for ci30repo's own branch.
_ci30_below=$(( $(date +%s) - 100 ))   # 100s < default 600s threshold
cat > "$STUB_FORGE" <<FEOF30A
#!/usr/bin/env bash
cmd="\${1:-}"; branch_arg="\${3:-}"
case "\$cmd:\$branch_arg" in
    batch-ci-status:*ci30*) printf 'run-id: 1\nqueued-since: ${_ci30_below}\n' ;;
esac
exit 0
FEOF30A
chmod +x "$STUB_FORGE"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."*
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-stalled: DETECTED=no when queued only 100s (threshold 600s)" \
    "CLASS=ci-stalled DETECTED=no" "$_log"

cat > "$STUB_FORGE" <<'FEOF30B'
#!/usr/bin/env bash
cmd="${1:-}"
case "$cmd" in
    batch-ci-status) exit 0 ;;
esac
exit 0
FEOF30B
chmod +x "$STUB_FORGE"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-stalled: DETECTED=no when forge returns no queued-since (nothing queued)" \
    "CLASS=ci-stalled DETECTED=no" "$_log"

# A missing branch= means read_branch() returns None and the loop skips this open dir
# before ever calling forge.sh for it — so the stub is never queried for ci30repo at all;
# it must still answer nothing for every OTHER open repo, or one of those would fire instead.
printf 'opened_at=100\n' > "$_ci30_dir/open"   # malformed: no branch= field
cat > "$STUB_FORGE" <<'FEOF30C'
#!/usr/bin/env bash
exit 0
FEOF30C
chmod +x "$STUB_FORGE"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-stalled: DETECTED=no when the open file has no branch= field" \
    "CLASS=ci-stalled DETECTED=no" "$_log"
printf 'branch=spira/queue/ci30-test\n' > "$_ci30_dir/open"   # restore for later sections

# ==========================================================================================
printf '\n%s\n' "32. ci-stalled: a forge call failure is unobservable, never satisfied"
# ==========================================================================================
# POSITIVE CONTROL is test 4 (forge succeeds, empty → DETECTED=no STATUS=satisfied). Here
# the forge call itself fails (non-zero exit): the old code read that identically to "no CI
# activity" (empty stdout either way) and reported DETECTED=no — silently treating a broken
# instrument as a quiet queue. STATUS=unobservable must appear instead, and it must never
# be STATUS=satisfied while the forge seam cannot be read.
cat > "$STUB_FORGE" <<'FEOF32'
#!/usr/bin/env bash
cmd="${1:-}"
case "$cmd" in
    batch-ci-status) exit 1 ;;
    *) exit 0 ;;
esac
FEOF32
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_cis_line="$(printf '%s\n' "$_log" | grep 'CLASS=ci-stalled ' | tail -1)"
want "ci-stalled: STATUS=unobservable when the forge call fails" "STATUS=unobservable" "$_cis_line"
lack "ci-stalled: never STATUS=satisfied when the forge call fails" "STATUS=satisfied" "$_cis_line"

# ==========================================================================================
printf '\n%s\n' "33. ci-stalled: a remedy that doesn't close its gap escalates on the next pass"
# ==========================================================================================
_now_e33="$(date +%s)"
_old_queued33=$(( _now_e33 - 700 ))
mkdir -p "$SPIRA_RUN/queue/remedyrepo"
printf 'branch=spira/queue/remedy-test\n' > "$SPIRA_RUN/queue/remedyrepo/open"
mkdir -p "$T/remedyrepo"
printf 'remedyrepo | %s | push | origin/main | |\n' "$T/remedyrepo" >> "$SPIRA_REPO_MAP"

# Scoped to remedy-test's own branch so the other repos left open by earlier sections
# (testrepo, redrepo, baseredrepo) stay quiet and cannot steal this pass's aggregate
# REMEDY=/TIER= fields — find_open_files has no ordering guarantee across repos.
cat > "$STUB_FORGE" <<FEOF33
#!/usr/bin/env bash
cmd="\${1:-}"; branch_arg="\${3:-}"
case "\$cmd" in
    batch-ci-status)
        if [ "\$branch_arg" = "spira/queue/remedy-test" ]; then
            printf 'run-id: 77777\n'
            printf 'queued-since: ${_old_queued33}\n'
        fi
        ;;
    workflow-rerun) printf 'stub: workflow-rerun %s\n' "\${3:-}" >> "\$FORGE_LOG" ;;
esac
exit 0
FEOF33
chmod +x "$STUB_FORGE"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG" "$FORGE_LOG"
SPIRA_CZAR_STAGE_CI_STALLED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_forge_calls_1="$(cat "$FORGE_LOG" 2>/dev/null || true)"
want "remedy pass 1: ci-stalled DETECTED=yes" "CLASS=ci-stalled DETECTED=yes" "$_log"
want "remedy pass 1: deterministic rerun attempted" "REMEDY=det-rerun" "$_log"
want "remedy pass 1: workflow-rerun called" "workflow-rerun" "$_forge_calls_1"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG" "$FORGE_LOG"
SPIRA_CZAR_STAGE_CI_STALLED=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
_forge_calls_2="$(cat "$FORGE_LOG" 2>/dev/null || true)"
want "remedy pass 2: still stalled, still DETECTED=yes" "CLASS=ci-stalled DETECTED=yes" "$_log"
want "remedy pass 2: escalates instead of retrying" "REMEDY=inference" "$_log"
want "remedy pass 2: incident filed with cause=ci-stalled" "cause=ci-stalled" "$_inc_log"
lack "remedy pass 2: does not rerun the workflow a second time" "workflow-rerun" "$_forge_calls_2"

# ==========================================================================================
printf '\n%s\n' "34. pool-idle: the certified-pool trigger's harness-owned replacement (sp-forah)"
# ==========================================================================================
# A real repo, unlike the log/forge-only detectors above: pool-idle shells to
# `git for-each-ref`, so the branches it counts have to actually exist.
POOL_REPO="$T/poolrepo"
mkdir -p "$POOL_REPO"
(
    cd "$POOL_REPO" \
        && git init -q -b main \
        && GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
           git commit -q --allow-empty -m base
)
printf 'poolrepo | %s | queue | origin/main | |\n' "$POOL_REPO" >> "$SPIRA_REPO_MAP"

# Pinned to non-default values (both default elsewhere: floor 4, window 900s) so this
# proves the config keys are wired, not just their Rust-side fallbacks.
export SPIRA_QUEUE_ROUND_MIN_N=3
export SPIRA_QUEUE_ROUND_STALL_SECS=120

mkdir -p "$SPIRA_RUN/landstate"
LC_BIN="$T/lc-bin"; LC_ROWS="$T/lc-certified-rows"
mkdir -p "$LC_BIN"; : > "$LC_ROWS"
cat > "$LC_BIN/spira-lc" <<LCEOF
#!/usr/bin/env bash
[ "\${1:-}" = list ] || exit 0
printf '['; sep=''
while read -r id; do printf '%s{"bead_id":"%s","state":"CERTIFIED"}' "\$sep" "\$id"; sep=','; done < "$LC_ROWS"
printf ']\n'
LCEOF
chmod +x "$LC_BIN/spira-lc"
export PATH="$LC_BIN:$PATH"
certify_pool_member() {
    git -C "$POOL_REPO" branch "spira/$1"
    printf 'CERTIFIED deadbeef %s\n' "$(date +%s)" > "$SPIRA_RUN/landstate/$1"
    printf '%s\n' "$1" >> "$LC_ROWS"
}

# POSITIVE CONTROL: two certified members is below the floor of 3 — must stay quiet.
certify_pool_member sp-poola1
certify_pool_member sp-poola2
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG" "$SPIRA_RUN/reconciler-state.json"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "pool-idle: DETECTED=no below the floor (2 < 3)" "CLASS=pool-idle DETECTED=no" "$_log"

# POSITIVE CONTROL: a third member reaches the floor, but an open batch is deliberate
# back-pressure — must stay quiet however full the pool.
certify_pool_member sp-poola3
mkdir -p "$SPIRA_RUN/queue/poolrepo"
printf 'branch=spira/queue/x\n' > "$SPIRA_RUN/queue/poolrepo/open"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG" "$SPIRA_RUN/reconciler-state.json"
"$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "pool-idle: DETECTED=no with a batch already open, at the floor" "CLASS=pool-idle DETECTED=no" "$_log"
rm -f "$SPIRA_RUN/queue/poolrepo/open"

# SEEN RED: at the floor, no batch open, streak seeded past the window — must fire and
# file an incident naming the repo and its depth. Before this detector, nothing here
# alarmed on this shape at all (sp-ji62y: a chat-session watcher died on compaction and
# 23 certified beads sat idle for ~30 minutes with no alarm).
_now_e34="$(date +%s)"
_old_since34=$(( _now_e34 - 121 ))   # 121s > SPIRA_QUEUE_ROUND_STALL_SECS=120
python3 -c "
import json
st = {'pool-idle:poolrepo': {'since': $_old_since34, 'remedy_attempted_at': None, 'remedy_desc': None}}
print(json.dumps(st))
" > "$SPIRA_RUN/reconciler-state.json"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG"
SPIRA_CZAR_STAGE_POOL_IDLE=act "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "pool-idle: DETECTED=yes once the streak outlasts the window" "CLASS=pool-idle DETECTED=yes" "$_log"
want "pool-idle: incident filed with cause=pool-idle" "cause=pool-idle" "$_inc_log"
want "pool-idle: subject names the repo" "poolrepo" "$_inc_log"
want "pool-idle: subject names the depth" "(3)" "$_inc_log"

unset SPIRA_QUEUE_ROUND_MIN_N SPIRA_QUEUE_ROUND_STALL_SECS
rm -f "$SPIRA_RUN/reconciler-state.json"

tl_summary
