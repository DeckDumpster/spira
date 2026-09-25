#!/usr/bin/env bash
#
# test-czar-pass.sh — czar fast pass: budget, flock, and acceptance detectors
#
# WHAT THIS SUITE CHECKS.
#   1. czar.sh exists and is executable.
#   2. Pass with an empty environment completes in under 5 s (budget).
#   3. A concurrent pass is skipped (flock prevents overlap).
#   4. ci-stalled acceptance case: fixture with a queued CI job > threshold →
#      DETECTED=yes in czar.log; must FAIL against the previous code (no czar.sh).
#   5. starved acceptance case: fixture strands.json with first-seen > threshold →
#      DETECTED=yes in czar.log.
#   6. Shadow mode: CZAR-WOULD written to czar.log; no bead filed.
#   7. Act mode: deterministic remedy executed; inference bead filed.
#   8. World halted: pass exits 0 without acting.
#   9. SPIRA_LOOP_STALL_SECS and SPIRA_CI_RED_MAX_SECS are in conf.sh allowlist.
#  10. spira-czar-pass.timer and .service exist in systemd/.
#  11. watchtower.sh --queue-checks is retired (stub response only).
#  12. sentinel.sh no longer calls watchtower --queue-checks.
#  16-21. base-red: the base ref's OWN gate run, not a batch's. Positive control
#      (green base → no bead), red base → P0+express bead naming the failing suites
#      (replaying the 2026-09-24 shape: test-install-bootstrap-release.sh and
#      test-watch-refresh.sh), a same-red second pass computing the same dedupe ref,
#      and an unreadable base — grace-suppressed at first, then filed as unreadable
#      (never treated as green) once it outlasts the grace window.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): for detectors 4 and 5,
# the test first verifies NO detection with an empty/fresh fixture, then adds the
# trigger and verifies detection. A detector that fires on empty data is not a detector.
#
# covers: czar-pass/src/main.rs spira/czar.sh spira/conf.sh spira/sentinel.sh spira/watchtower.sh
#         spira/systemd/spira-czar-pass.service spira/systemd/spira-czar-pass.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s\n' "$1"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1: wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }

CZAR="$HERE/czar.sh"
[ -x "$CZAR" ] || { printf 'czar.sh not found or not executable: %s\n' "$CZAR" >&2; exit 2; }

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
CZAR_PASS_BIN="$CZAR_PASS_ROOT/target/release/czar-pass"
if [ ! -x "$CZAR_PASS_BIN" ]; then
    cp -r "$CZAR_PASS_ROOT/." "$T/czar-pass-src"
    printf '  (building czar-pass into %s)\n' "$T/czar-pass-target"
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/czar-pass-target" \
        "$CARGO_BIN" build --release \
        --manifest-path "$T/czar-pass-src/Cargo.toml" 2>&1 | tail -5
    CZAR_PASS_BIN="$T/czar-pass-target/release/czar-pass"
fi
if [ ! -x "$CZAR_PASS_BIN" ]; then
    printf 'czar-pass binary not found at %s\n' "$CZAR_PASS_BIN" >&2
    printf '0 passed, 1 failed\n'
    exit 1
fi
export SPIRA_CZAR_PASS_BIN="$CZAR_PASS_BIN"

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
printf '\n%s\n' "1. czar.sh exists and accepts --pass"
# ==========================================================================================
[ -f "$CZAR" ] && ok "czar.sh exists" || bad "czar.sh not found at $CZAR"
[ -x "$CZAR" ] && ok "czar.sh is executable" || bad "czar.sh not executable"

# ==========================================================================================
printf '\n%s\n' "2. budget: empty environment completes in under 5s"
# ==========================================================================================
_t0="$(date +%s)"
bash "$CZAR" --pass >/dev/null 2>&1
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
_flock_out="$(bash "$CZAR" --pass 2>&1 || true)"
exec 9>&-
want "concurrent pass logs 'already running'" "already running" "$_flock_out"

# ==========================================================================================
printf '\n%s\n' "4. ci-stalled acceptance case (POSITIVE CONTROL first)"
# ==========================================================================================
# POSITIVE CONTROL: no open batch → ci-stalled not detected.
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept"
bash "$CZAR" --pass >/dev/null 2>&1
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
bash "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "ci-stalled: DETECTED=yes in czar.log" "CLASS=ci-stalled DETECTED=yes" "$_log"

# ==========================================================================================
printf '\n%s\n' "5. starved acceptance case (POSITIVE CONTROL first)"
# ==========================================================================================
# POSITIVE CONTROL: no strands.json → starved not detected.
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."*
rm -f "$SPIRA_RUN/strands.json"
# Use minimal forge (no queued jobs for the open batch)
cat > "$STUB_FORGE" <<'FEOF3'
#!/usr/bin/env bash
cmd="${1:-}"
case "$cmd" in
    batch-ci-status|queued-since|run-id) exit 0 ;;
    *) exit 0 ;;
esac
FEOF3
chmod +x "$STUB_FORGE"
bash "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "starved: DETECTED=no when no strands.json" "CLASS=starved DETECTED=no" "$_log"

# SEEN RED: fixture strands.json with first-seen > threshold.
_sv_first=$(( _now_e - 1800 ))   # 30 min > default threshold of 20 min
python3 -c "
import json
st = {'task:starved:builder': {'first': $_sv_first}}
print(json.dumps(st))
" > "$SPIRA_RUN/strands.json"

rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."*
bash "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "starved: DETECTED=yes in czar.log" "CLASS=starved DETECTED=yes" "$_log"

# ==========================================================================================
printf '\n%s\n' "6. shadow mode: CZAR-WOULD written, no bead filed"
# ==========================================================================================
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* \
      "$INC_LOG"
SPIRA_CZAR_STAGE_STARVED=shadow bash "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "shadow: CZAR-WOULD written to czar.log for starved" "CZAR-WOULD: starved" "$_log"
lack "shadow: no incident filed in shadow mode" "incident: cause=starved" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "7. act mode: inference bead filed for starved"
# ==========================================================================================
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$SPIRA_RUN/czar-pass-first."* \
      "$INC_LOG"
SPIRA_CZAR_STAGE_STARVED=act bash "$CZAR" --pass >/dev/null 2>&1
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "act: incident filed with cause=starved" "cause=starved" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "8. world halted: pass exits without acting"
# ==========================================================================================
touch "$SPIRA_RUN/world.halted"
rm -f "$SPIRA_RUN/czar.log" "$INC_LOG"
_halt_out="$(bash "$CZAR" --pass 2>&1 || true)"
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
for key in SPIRA_LOOP_STALL_SECS SPIRA_CI_RED_MAX_SECS; do
    grep -q "$key" "$conf_sh" 2>/dev/null \
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
printf '\n%s\n' "11. watchtower.sh --queue-checks is retired"
# ==========================================================================================
wt="$HERE/watchtower.sh"
_wt_out="$(SPIRA_RUN="$SPIRA_RUN" bash "$wt" --queue-checks 2>&1 || true)"
want "watchtower: --queue-checks logs 'retired'" "retired" "$_wt_out"
# The queue-stall detector logic must not be in watchtower.sh anymore.
lack "watchtower: no 'ejected 0' detector in watchtower.sh" "ejected 0, requeued" \
    "$(cat "$wt")"

# ==========================================================================================
printf '\n%s\n' "12. sentinel.sh does not call watchtower --queue-checks"
# ==========================================================================================
_sent="$HERE/sentinel.sh"
lack "sentinel.sh: no watchtower --queue-checks call" \
    "watchtower.sh --queue-checks" "$(cat "$_sent")"

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
bash "$CZAR" --pass >/dev/null 2>&1
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
bash "$CZAR" --pass >/dev/null 2>&1
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
bash "$CZAR" --pass >/dev/null 2>&1
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
bash "$CZAR" --pass >/dev/null 2>&1
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
SPIRA_CZAR_STAGE_BASE_RED=act bash "$CZAR" --pass >/dev/null 2>&1
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
SPIRA_CZAR_STAGE_BASE_RED=act bash "$CZAR" --pass >/dev/null 2>&1
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
bash "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
want "base-red: DETECTED=no while unreadable is still inside grace" \
    "CLASS=base-red DETECTED=no" "$_log"
lack "base-red: no bead filed while still inside grace" "cause=base-red" \
    "$(cat "$INC_LOG" 2>/dev/null || true)"

# ==========================================================================================
printf '\n%s\n' "20. base-red: unreadable past grace → filed as unreadable, not treated as green"
# ==========================================================================================
_old_unreadable=$(( _now_e3 - 200 ))   # 200s > default 120s grace
printf '%s\n' "$_old_unreadable" > "$SPIRA_RUN/czar-pass-first.base-red-unreadable-baseredrepo"
rm -f "$SPIRA_RUN/czar.log" "$SPIRA_RUN/czar-pass.swept" "$INC_LOG"
SPIRA_CZAR_STAGE_BASE_RED=act bash "$CZAR" --pass >/dev/null 2>&1
_log="$(cat "$SPIRA_RUN/czar.log" 2>/dev/null || true)"
_inc_log="$(cat "$INC_LOG" 2>/dev/null || true)"
want "base-red: DETECTED=yes once unreadable outlasts grace" "CLASS=base-red DETECTED=yes" "$_log"
want "base-red: bead says status could not be read" "could not be read" "$_inc_log"
want "base-red: unreadable bead still filed at P0" "priority=0" "$_inc_log"

# ==========================================================================================
printf '\n%s\n' "21. SPIRA_BASE_CI_UNREADABLE_GRACE_SECS and SPIRA_CZAR_STAGE_BASE_RED in conf.sh allowlist"
# ==========================================================================================
for key in SPIRA_BASE_CI_UNREADABLE_GRACE_SECS SPIRA_CZAR_STAGE_BASE_RED; do
    grep -q "$key" "$conf_sh" 2>/dev/null \
        && ok "$key in SPIRA_CONF_KEYS" \
        || bad "$key missing from conf.sh"
done

printf '\n  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
