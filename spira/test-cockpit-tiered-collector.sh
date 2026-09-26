#!/usr/bin/env bash
#
# test-cockpit-tiered-collector.sh — tiered cockpit collector fragment lifecycle.
#
#   ./test-cockpit-tiered-collector.sh
#
# WHAT THIS SUITE TESTS:
#
#   1. NEVER → ? (positive control first): a probe fragment with status=never contributes
#      no value keys, so health.sh's ${SP_FOO:-?} renders '?' for those keys.
#
#   2. OK → value: a probe fragment with status=ok and value keys merges those values into
#      cockpit.env.
#
#   3. _PROBE_AT_<name> appears in the merged file with the fragment's timestamp.
#
#   4. STALE retains values: a probe fragment with status=stale keeps its previous values
#      so the pane renders last-known-good rather than blanking (blanking reads as 0 on a
#      pane that lays rows by position — the defect sp-xrkuu fixed).
#
#   5. FIRST-WINS per key: the fast probe's copy of a key wins over the slow probe's copy
#      (alphabetical fragment order: 'now.env' sorts before 'unsent.env').
#
#   6. SUPERVISION: collect.sh loop refuses without INVOCATION_ID, same guard as cockpit.sh.
#
# covers: spira/collect.sh spira/cockpit.sh

# covers: spira/collect.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'chmod -R +w "$TMP" 2>/dev/null || true; rm -rf "$TMP"' EXIT
FRAG_DIR="$TMP/cockpit.d"
SNAP="$TMP/cockpit.env"
mkdir -p "$FRAG_DIR"
BASE_PATH="$PATH"

# Write a fragment file atomically, same way collect.sh does.
write_frag() {
    local name="$1" status="$2" at="$3"; shift 3
    {
        printf '_PROBE_AT=%s\n' "$at"
        printf '_PROBE_STATUS=%s\n' "$status"
        printf '%s\n' "$@"
    } > "$FRAG_DIR/${name}.env"
}

# Merge fixture fragments through the real collect.sh, not a copy of its logic.
run_merge() {
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        SPIRA_COCKPIT="$TMP" FRAG_DIR="$FRAG_DIR" \
        bash "$HERE/collect.sh" merge 2>/dev/null
}

# ============================================================
echo "1. never → ? (positive control first):"

# POSITIVE CONTROL: probe with status=ok should produce a value.
write_frag "now" "ok" "1000000" "SP_AT=1000000" "SP_WINDOW_HOURS=24"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "status=ok probe: SP_AT present"           "SP_AT="           "$snap"
want "status=ok probe: SP_WINDOW_HOURS present"  "SP_WINDOW_HOURS=" "$snap"

# Now overwrite with never status.
write_frag "now" "never" "0"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
nowant "status=never probe: SP_AT absent from values"          "SP_AT="          "$snap"
nowant "status=never probe: SP_WINDOW_HOURS absent from values" "SP_WINDOW_HOURS=" "$snap"
# But the meta keys must still be present.
want "status=never probe: _PROBE_AT_now present"   "_PROBE_AT_now="   "$snap"
want "status=never probe: _PROBE_STATUS_now present" "_PROBE_STATUS_now='never'" "$snap"

# ============================================================
echo
echo "2. ok → value:"

rm -f "$FRAG_DIR"/*.env
write_frag "core" "ok" "1000001" "SP_READY=5" "SP_NEXT_N=5"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "status=ok: SP_READY present"        "SP_READY=" "$snap"
want "status=ok: _PROBE_AT_core present"  "_PROBE_AT_core=" "$snap"
want "status=ok: _PROBE_STATUS_core=ok"   "_PROBE_STATUS_core='ok'" "$snap"

# ============================================================
echo
echo "3. _PROBE_AT_<name> matches fragment timestamp:"

rm -f "$FRAG_DIR"/*.env
EPOCH=1725000000
write_frag "sphere" "ok" "$EPOCH" "SP_SPHERE_N=3"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "_PROBE_AT_sphere matches epoch" "_PROBE_AT_sphere='$EPOCH'" "$snap"

# ============================================================
echo
echo "4. stale retains values:"

rm -f "$FRAG_DIR"/*.env
# First write a good fragment.
write_frag "unsent" "ok" "1000002" "SP_UNSENT=7" "SP_UNSENT_OLDEST_H=3"
run_merge
snap_before="$(cat "$SNAP" 2>/dev/null)"
want "ok fragment: SP_UNSENT present before stale" "SP_UNSENT=" "$snap_before"

# Now write the stale version (simulating a failed probe that kept old values).
write_frag "unsent" "stale" "1000002" "SP_UNSENT=7" "SP_UNSENT_OLDEST_H=3"
run_merge
snap_after="$(cat "$SNAP" 2>/dev/null)"
want "stale fragment: SP_UNSENT still present"         "SP_UNSENT=" "$snap_after"
want "stale fragment: _PROBE_STATUS_unsent=stale"      "_PROBE_STATUS_unsent='stale'" "$snap_after"

# ============================================================
echo
echo "5. first-wins: earlier alphabetical probe wins shared key:"

rm -f "$FRAG_DIR"/*.env
# 'now.env' < 'unsent.env' alphabetically. Both emit SP_AT.
write_frag "now"    "ok" "1000003" "SP_AT=1000003"
write_frag "unsent" "ok" "1000004" "SP_AT=9999999"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "first-wins: now's SP_AT wins"        "SP_AT='1000003'" "$snap"
nowant "first-wins: unsent's SP_AT absent" "SP_AT='9999999'" "$snap"

# ============================================================
echo
echo "6. supervision: collect.sh loop refuses without INVOCATION_ID:"

BASE_PATH="$PATH"
loop_out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    bash "$HERE/collect.sh" loop 2>&1)" || true
want "collect.sh loop: refusal message present" "not the supervised process" "$loop_out"

# ============================================================
# cockpit.sh's own SP_AT ordering and unsupervised-refusal are test-cockpit.sh's job
# (cluster 7, docs/test-plan/cockpit-observability.md) — not duplicated here.

echo
echo "7. timeout status: merge treats it like never (no value keys), _probe_body_test writes it:"

# Positive control first: a fragment with status=ok contributes its value keys.
rm -f "$FRAG_DIR"/*.env
write_frag "tprobe" "ok" "1000010" "SP_TVAL=42"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
want "timeout/positive control: ok contributes SP_TVAL" "SP_TVAL=" "$snap"

# Now write the same fragment with status=timeout (including a value key to prove the
# merge exclusion is active, not just that the fragment happens to have no values).
write_frag "tprobe" "timeout" "0" "SP_TVAL=99"
run_merge
snap="$(cat "$SNAP" 2>/dev/null)"
nowant "timeout: SP_TVAL absent (no values from timed-out probe)" "SP_TVAL=" "$snap"
want "timeout: _PROBE_STATUS_tprobe=timeout" "_PROBE_STATUS_tprobe='timeout'" "$snap"

# _probe_body_test: a probe that exits 124 (timeout's exit code) on its first run must
# write _PROBE_STATUS=timeout to its fragment. POSITIVE CONTROL: a succeeding probe first.
MOCK_COCK="$TMP/mock-cockpit.sh"
printf '#!/usr/bin/env bash\ncase "$1" in succeed) echo SP_MOCK=1 ;; slow) exec sleep 300 ;; esac\n' \
    > "$MOCK_COCK" && chmod +x "$MOCK_COCK"

rm -f "$FRAG_DIR"/*.env
# Positive control: succeeding probe writes _PROBE_STATUS=ok.
printf '_PROBE_AT=0\n_PROBE_STATUS=never\n' > "$FRAG_DIR/tprobe2.env"
BASE_PATH="$PATH"
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    FRAG_DIR="$FRAG_DIR" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test tprobe2 10 succeed 2>/dev/null || true
frag2="$(cat "$FRAG_DIR/tprobe2.env" 2>/dev/null)"
want "_probe_body_test/ok: _PROBE_STATUS=ok" "_PROBE_STATUS=ok" "$frag2"

# First-run timeout: probe that runs slowly and is killed after 1s writes _PROBE_STATUS=timeout.
printf '_PROBE_AT=0\n_PROBE_STATUS=never\n' > "$FRAG_DIR/tprobe3.env"
tout_log="$TMP/probe_timeout.log"
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    FRAG_DIR="$FRAG_DIR" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test tprobe3 1 slow > /dev/null 2>"$tout_log" || true
frag3="$(cat "$FRAG_DIR/tprobe3.env" 2>/dev/null)"
want "_probe_body_test/timeout: _PROBE_STATUS=timeout in fragment" "_PROBE_STATUS=timeout" "$frag3"
tout_msg="$(cat "$tout_log" 2>/dev/null)"
want "_probe_body_test/timeout: journal line mentions probe name" "tprobe3" "$tout_msg"
want "_probe_body_test/timeout: journal line mentions timeout" "timeout" "$tout_msg"

# ============================================================
echo
echo "8. exit on consecutive merge failures (positive control first):"

# POSITIVE CONTROL: merge succeeds (SPIRA_RUN writable) → loop keeps running.
# Run the loop in the background for a short time; it must NOT exit prematurely.
LOOP_RUN="$TMP/loop_run"
mkdir -p "$LOOP_RUN"
LOOP_FRAG="$LOOP_RUN/cockpit.d"
mkdir -p "$LOOP_FRAG"
BASE_PATH="$PATH"
timeout 3 env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$LOOP_RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    SPIRA_COCKPIT_FORCE=1 \
    SPIRA_COCKPIT_TICK=0 \
    SPIRA_COCKPIT_MERGE_FAIL_MAX=2 \
    FRAG_DIR="$LOOP_FRAG" \
    bash "$HERE/collect.sh" loop >/dev/null 2>&1 || _ec=$?
# timeout exits 124 when the process was still running; that is the passing case here.
if [ "${_ec:-0}" -eq 124 ]; then
    ok "loop keeps running when merge succeeds (not killed prematurely)"
else
    bad "loop exited ${_ec:-?} while merge should succeed" "wanted timeout (124)"
fi

# FAILURE CASE: merge always fails because SPIRA_RUN is read-only.
# The loop must exit non-zero after _MERGE_FAIL_MAX consecutive failures.
LOOP_READONLY="$TMP/loop_readonly"
mkdir -p "$LOOP_READONLY"
LOOP_READONLY_FRAG="$LOOP_READONLY/cockpit.d"
mkdir -p "$LOOP_READONLY_FRAG"
# Create the never-fragments so _write_never_frag does not need to write anything.
for _pn in now sphere repo_labels strands ratelim core core_detail sops livelock dup_refs unsent; do
    printf '_PROBE_AT=0\n_PROBE_STATUS=never\n' > "$LOOP_READONLY_FRAG/${_pn}.env"
done
# Make SPIRA_RUN (not FRAG_DIR) read-only so mktemp "$SPIRA_RUN/.cockpit.XXXXXX" fails.
chmod 555 "$LOOP_READONLY"
_loop_rc=0
timeout 5 env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$LOOP_READONLY" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    SPIRA_COCKPIT_FORCE=1 \
    SPIRA_COCKPIT_TICK=0 \
    SPIRA_COCKPIT_MERGE_FAIL_MAX=2 \
    FRAG_DIR="$LOOP_READONLY_FRAG" \
    bash "$HERE/collect.sh" loop >/dev/null 2>&1 || _loop_rc=$?
chmod 755 "$LOOP_READONLY"
if [ "$_loop_rc" -eq 1 ]; then
    ok "loop exits 1 after consecutive merge failures"
else
    bad "loop should exit 1 on merge failures" "got exit code ${_loop_rc}"
fi

# Verify the exit message is written to stderr.
_loop_err=""
chmod 555 "$LOOP_READONLY"
timeout 5 env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$LOOP_READONLY" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    SPIRA_COCKPIT_FORCE=1 \
    SPIRA_COCKPIT_TICK=0 \
    SPIRA_COCKPIT_MERGE_FAIL_MAX=2 \
    FRAG_DIR="$LOOP_READONLY_FRAG" \
    bash "$HERE/collect.sh" loop >/dev/null 2>"$TMP/loop_merge_err.log" || true
chmod 755 "$LOOP_READONLY"
_loop_err="$(cat "$TMP/loop_merge_err.log" 2>/dev/null)"
want "loop exit message names merge failures" "consecutive merge failures" "$_loop_err"

# Config-change exit is now covered by test-conf-watch.sh, alongside health.sh and
# loom.sh's own loops (duplicate cluster 9, docs/test-plan/cockpit-observability.md).

# ============================================================
echo
echo "9. SP_COLLECTOR_REV stamped in merged snapshot:"

# POSITIVE CONTROL: a probe with ok status contributes values so the merge ran at all.
rm -f "$FRAG_DIR"/*.env
printf '_PROBE_AT=1\n_PROBE_STATUS=ok\nSP_AT=1\n' > "$FRAG_DIR/now.env"
BASE_PATH="$PATH"
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" FRAG_DIR="$FRAG_DIR" \
    bash "$HERE/collect.sh" merge 2>/dev/null
snap="$(cat "$SNAP" 2>/dev/null)"
want "SP_COLLECTOR_REV: positive control — SP_AT still present (merge worked)" "SP_AT=" "$snap"
want "SP_COLLECTOR_REV: present in snapshot" "SP_COLLECTOR_REV=" "$snap"
# The value must not be blank (git failures produce 'unknown', not '').
if [[ "$snap" != *"SP_COLLECTOR_REV=''"* ]]; then
    ok "SP_COLLECTOR_REV: value is non-empty"
else
    bad "SP_COLLECTOR_REV: blank value" "wanted a rev or 'unknown', got empty string"
fi

# ============================================================
echo
echo "10. temp cleanup: external kill leaves no temp in cockpit.d:"

# POSITIVE CONTROL: a slow probe in a subshell creates a temp before being killed.
# Kill it and verify the temp is gone.
KILL_FRAG="$TMP/kill_frag"
mkdir -p "$KILL_FRAG"
MOCK_COCK2="$TMP/mock-cockpit2.sh"
printf '#!/usr/bin/env bash\ncase "$1" in slow) exec sleep 300 ;; esac\n' \
    > "$MOCK_COCK2" && chmod +x "$MOCK_COCK2"
# Start the probe in a subshell so we can observe mid-run state and kill it.
(
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        SPIRA_COCKPIT="$TMP" \
        FRAG_DIR="$KILL_FRAG" COCK="$MOCK_COCK2" \
        bash "$HERE/collect.sh" _probe_body_test killtest 30 slow 2>/dev/null
) &
kill_pid=$!

# Positive control: wait for the temp to appear.
kill_seen=0
for _ in $(seq 1 100); do
    if [ "$(find "$KILL_FRAG" -maxdepth 1 -name '.*' | wc -l)" -gt 0 ]; then
        kill_seen=1; break
    fi
    kill -0 "$kill_pid" 2>/dev/null || break
    sleep 0.1
done
if [ "$kill_seen" -eq 1 ]; then
    ok "12/kill: temp present mid-probe (positive control)"
else
    bad "12/kill: temp never appeared — probe did not create it" "cannot test cleanup"
fi

kill -TERM "$kill_pid" 2>/dev/null; wait "$kill_pid" 2>/dev/null || true
n_kill_tmps="$(find "$KILL_FRAG" -maxdepth 1 -name '.*' | wc -l)"
[ "$n_kill_tmps" -eq 0 ] && ok "12/kill: no temp left after SIGTERM" \
    || bad "12/kill: $n_kill_tmps temp(s) remained after SIGTERM" "EXIT trap failed"

# ============================================================
echo
echo "11. killed counter: timeout increments _PROBE_KILLED, success resets to 0:"

KILLED_FRAG="$TMP/killed_frag"
mkdir -p "$KILLED_FRAG"
printf '_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n' > "$KILLED_FRAG/ktest.env"
# First timeout: probe sleeps, 1s timeout fires.
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    FRAG_DIR="$KILLED_FRAG" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test ktest 1 slow 2>/dev/null || true
frag_k1="$(cat "$KILLED_FRAG/ktest.env" 2>/dev/null)"
want "13/first timeout: _PROBE_KILLED=1" "_PROBE_KILLED=1" "$frag_k1"
# Verify no temp file remains after the timeout.
n_k1_tmps="$(find "$KILLED_FRAG" -maxdepth 1 -name '.*' | wc -l)"
[ "$n_k1_tmps" -eq 0 ] && ok "13/first timeout: no temp left" \
    || bad "13/first timeout: $n_k1_tmps temp(s) left" "EXIT trap failed"

# Second timeout: counter increments again.
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    FRAG_DIR="$KILLED_FRAG" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test ktest 1 slow 2>/dev/null || true
frag_k2="$(cat "$KILLED_FRAG/ktest.env" 2>/dev/null)"
want "13/second timeout: _PROBE_KILLED=2" "_PROBE_KILLED=2" "$frag_k2"

# Success resets counter to 0.
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    FRAG_DIR="$KILLED_FRAG" COCK="$MOCK_COCK" \
    bash "$HERE/collect.sh" _probe_body_test ktest 10 succeed 2>/dev/null || true
frag_k3="$(cat "$KILLED_FRAG/ktest.env" 2>/dev/null)"
want "13/success: _PROBE_KILLED=0" "_PROBE_KILLED=0" "$frag_k3"

# ============================================================
echo
echo "12. startup sweep: old temps in cockpit.d removed on loop start:"

SWEEP_RUN="$TMP/sweep_run"
mkdir -p "$SWEEP_RUN"
SWEEP_FRAG="$SWEEP_RUN/cockpit.d"
mkdir -p "$SWEEP_FRAG"
# Create a temp file whose mtime is well past the max probe timeout (900s).
touch -d "1100 seconds ago" "$SWEEP_FRAG/.old_orphan.AbCdEf" 2>/dev/null \
    || { touch "$SWEEP_FRAG/.old_orphan.AbCdEf"; touch -t "$(date -d '1100 seconds ago' +%Y%m%d%H%M.%S 2>/dev/null || date +%Y%m%d%H%M.%S)" "$SWEEP_FRAG/.old_orphan.AbCdEf" 2>/dev/null; }
# Positive control: the file exists before the loop starts.
if [ -f "$SWEEP_FRAG/.old_orphan.AbCdEf" ]; then
    ok "14/sweep: old temp exists before loop (positive control)"
else
    bad "14/sweep: could not create old temp for positive control" "skip sweep test"
fi
for _pn in now sphere repo_labels strands ratelim core queue core_detail sops livelock dup_refs unsent; do
    printf '_PROBE_AT=0\n_PROBE_STATUS=never\n_PROBE_KILLED=0\n' > "$SWEEP_FRAG/${_pn}.env"
done
_sweep_ec=0
timeout 3 env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$SWEEP_RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    SPIRA_COCKPIT_FORCE=1 \
    SPIRA_COCKPIT_TICK=0 \
    FRAG_DIR="$SWEEP_FRAG" \
    bash "$HERE/collect.sh" loop >/dev/null 2>/dev/null || _sweep_ec=$?
if [ -f "$SWEEP_FRAG/.old_orphan.AbCdEf" ]; then
    bad "14/sweep: old temp still present after loop start" "sweep did not run"
else
    ok "14/sweep: old temp removed by startup sweep"
fi

# ============================================================
echo
echo "13. SP_PROBE_KILLED_<name> appears in merged snapshot:"

rm -f "$FRAG_DIR"/*.env
printf '_PROBE_AT=1\n_PROBE_STATUS=timeout\n_PROBE_KILLED=3\n' > "$FRAG_DIR/core.env"
env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" FRAG_DIR="$FRAG_DIR" \
    bash "$HERE/collect.sh" merge 2>/dev/null
snap="$(cat "$SNAP" 2>/dev/null)"
want "15/killed: SP_PROBE_KILLED_core=3 in snapshot" "SP_PROBE_KILLED_core='3'" "$snap"

# ============================================================
echo
tl_summary
