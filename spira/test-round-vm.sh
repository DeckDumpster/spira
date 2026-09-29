#!/usr/bin/env bash
#
# test-round-vm.sh — round-vm.sh's own logic: acquire/release/run against a fake provider,
# never a real Proxmox call and never a real VM.
#
#   ./test-round-vm.sh
#
# WHAT THIS PROVES (the unit tier of sp-7tw9h's Test strategy):
#   1. argument and credential errors name the missing variable
#   2. an unreachable provider makes acquire retry and alarm exactly ONCE per outage,
#      and never fabricates a VM to run on instead
#   3. acquire with a ready VM returns it and starts exactly one background provision
#   4. acquire with none records cold
#   5. every acquire hands out a freshly provisioned VM, never a reused one — so a file
#      left on a released VM is never visible to the next round, because there is no
#      "next round" that gets the same VM
#   6. the manifest step refuses to install binaries whose tree sha differs from the
#      one expected
#
# THE PROVIDER IS A FIXTURE, NOT A MODEL OF PROXMOX: it implements the exact three-function
# seam round-vm.sh calls (rvm_provider_provision/_destroy/_alive) and is swapped in via
# SPIRA_ROUND_VM_PROVIDER, the same seam a real EC2 provider would use — this is round-vm.sh's
# own contract under test, not a guess at Proxmox's.
#
# defect: sp-7tw9h
# tier: T1
# covers: spira/round-vm.sh spira/round-vm-provider-pve.sh
# hermetic-ok: no network calls, no systemd, no database, no real VM
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

printf 'test-round-vm.sh\n'

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

FAKE_HOME="$TMP/fake-home"
mkdir -p "$FAKE_HOME"
MAIL_LOG="$TMP/mail.log"
cat > "$FAKE_HOME/mail.sh" <<'SH'
#!/usr/bin/env bash
{ printf 'mail:'; printf ' %s' "$@"; printf '\n'; } >> "$MAIL_LOG_PATH"
exit 0
SH
chmod +x "$FAKE_HOME/mail.sh"

FAKE_DIR="$TMP/fake-provider-state"
FAKE_PROVIDER="$TMP/fake-provider.sh"
cat > "$FAKE_PROVIDER" <<'SH'
# fake round-vm provider — a controllable stand-in for round-vm-provider-pve.sh.
_FAKE_DIR="${FAKE_PROVIDER_DIR:?}"
mkdir -p "$_FAKE_DIR"

rvm_provider_provision() {
    printf 'provision\n' >> "$_FAKE_DIR/calls.log"
    local remaining=0
    [ -r "$_FAKE_DIR/fail-remaining" ] && remaining="$(cat "$_FAKE_DIR/fail-remaining")"
    if [ "$remaining" -gt 0 ]; then
        remaining=$((remaining - 1))
        printf '%s\n' "$remaining" > "$_FAKE_DIR/fail-remaining"
        printf '%s\n' "${FAKE_PROVIDER_FAIL_REASON:-stub: API unreachable}" >&2
        return 1
    fi
    local n=1
    [ -r "$_FAKE_DIR/seq" ] && n="$(cat "$_FAKE_DIR/seq")"
    printf '%s\n' "$((n + 1))" > "$_FAKE_DIR/seq"
    local handle="vm-$n"
    : > "$_FAKE_DIR/alive.$handle"
    printf '%s 10.0.0.%s\n' "$handle" "$n"
}

rvm_provider_destroy() {
    printf 'destroy %s\n' "$1" >> "$_FAKE_DIR/calls.log"
    rm -f "$_FAKE_DIR/alive.$1"
}

rvm_provider_alive() {
    [ -e "$_FAKE_DIR/alive.$1" ]
}
SH

RUN_DIR_N=0
# run_rvm <extra-env-assignments-as-one-string> -- <args...>
# A fresh SPIRA_RUN per call by default keeps acquire's on-disk state isolated between
# properties; a test that wants continuity across two calls passes the same STATE dir twice
# via SPIRA_ROUND_VM_STATE_DIR in extra_env.
run_rvm() {
    local extra_env="$1"; shift
    RUN_DIR_N=$((RUN_DIR_N + 1))
    env -i \
        PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$FAKE_HOME" \
        SPIRA_TOML="$TMP/no-such.toml" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_RUN="$TMP/run-$RUN_DIR_N" \
        SPIRA_ROUND_VM_PROVIDER="$FAKE_PROVIDER" \
        FAKE_PROVIDER_DIR="$FAKE_DIR" \
        MAIL_LOG_PATH="$MAIL_LOG" \
        SPIRA_ROUND_VM_RETRY_INTERVAL=0 \
        SPIRA_ROUND_VM_MAX_RETRIES=3 \
        $extra_env \
        bash "$HERE/round-vm.sh" "$@" 2>&1
}

# A dedicated persistent state dir for the tests that must observe continuity (background
# provisioning, warm handoff) across more than one acquire call.
POOL_STATE="$TMP/pool-state"

run_rvm_pool() {
    env -i \
        PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$FAKE_HOME" \
        SPIRA_TOML="$TMP/no-such.toml" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_RUN="$TMP/pool-run" \
        SPIRA_ROUND_VM_STATE_DIR="$POOL_STATE" \
        SPIRA_ROUND_VM_PROVIDER="$FAKE_PROVIDER" \
        FAKE_PROVIDER_DIR="$FAKE_DIR" \
        MAIL_LOG_PATH="$MAIL_LOG" \
        SPIRA_ROUND_VM_RETRY_INTERVAL=0 \
        SPIRA_ROUND_VM_MAX_RETRIES=3 \
        bash "$HERE/round-vm.sh" "$@" 2>&1
}

reset_fake() {
    rm -rf "$FAKE_DIR"; mkdir -p "$FAKE_DIR"
    : > "$MAIL_LOG"
}

# ---------------------------------------------------------------------------
# Positive control: `want` can fail. (law-absence-needs-a-positive-control)
# ---------------------------------------------------------------------------
if ( fail=0; want() { [[ "$3" == *"$2"* ]] || fail=1; }; want _ "not-present-at-all" "some other text"; exit "$fail" ); then
    bad "positive-control: want would fail on a non-match" "it did not"
else
    ok "positive-control: want would fail on a non-match"
fi

# ---------------------------------------------------------------------------
# 1. Argument and credential errors name the missing variable.
# ---------------------------------------------------------------------------
reset_fake
out="$(run_rvm "" release)"
want "release: no handle names the argument" "usage: round-vm.sh release" "$out"

out="$(run_rvm "" run)"
want "run: no tree-dir names the argument" "usage: round-vm.sh run" "$out"

out="$(run_rvm "" run "$TMP")"
want "run: not a git checkout names the path" "$TMP" "$out"

mkdir -p "$TMP/repo" && git -C "$TMP/repo" init --quiet -b main
git -C "$TMP/repo" -c user.email=t@example.com -c user.name=t commit --quiet --allow-empty -m init
out="$(run_rvm "" run "$TMP/repo")"
want "run: missing SPIRA_ROUND_VM_HOST_ADDR names the variable" "SPIRA_ROUND_VM_HOST_ADDR" "$out"

out="$(run_rvm "SPIRA_ROUND_VM_HOST_ADDR=10.0.0.1 SPIRA_ROUND_VM_HOST_KEY=$TMP/no-such-key" run "$TMP/repo")"
want "run: unreadable SPIRA_ROUND_VM_HOST_KEY names the path" "no-such-key" "$out"

out="$(run_rvm "SPIRA_ROUND_VM_PROVIDER=$TMP/no-such-provider.sh" acquire)"
want "acquire: unreadable provider names the path" "no-such-provider.sh" "$out"

# The real pve provider, exercised with an empty pve.env, names PVE_TEMPLATE_VMID —
# proves the credential-naming property against the shipped provider, not just the fixture.
: > "$TMP/empty-pve.env"
out="$(env -i PATH="$PATH" HOME="$TMP" SPIRA_HOME="$FAKE_HOME" \
    SPIRA_TOML="$TMP/no-such.toml" SPIRA_CONF="$TMP/no-such.conf" SPIRA_RUN="$TMP/run-pve" \
    SPIRA_PVE_ENV="$TMP/empty-pve.env" \
    SPIRA_ROUND_VM_PROVIDER="$HERE/round-vm-provider-pve.sh" \
    SPIRA_ROUND_VM_HOST_PUBKEY="$TMP/no-such-pubkey" \
    SPIRA_ROUND_VM_RETRY_INTERVAL=0 SPIRA_ROUND_VM_MAX_RETRIES=1 \
    bash "$HERE/round-vm.sh" acquire 2>&1)"
want "pve provider: missing PVE_TEMPLATE_VMID names the variable" "PVE_TEMPLATE_VMID" "$out"

# ---------------------------------------------------------------------------
# 2. An unreachable provider retries, alarms exactly once, and never fabricates a VM.
# The alarm's dedup lives in the state dir, so this section pins one across both calls —
# exactly as a real outage would span more than one acquire against the same pool.
# ---------------------------------------------------------------------------
reset_fake
OUTAGE_STATE="$TMP/outage-state"; rm -rf "$OUTAGE_STATE"
printf '3\n' > "$FAKE_DIR/fail-remaining"   # fails every attempt; MAX_RETRIES=3 exhausts it
out="$(run_rvm "SPIRA_ROUND_VM_STATE_DIR=$OUTAGE_STATE" acquire)"; rc=$?
is "outage: acquire fails rather than fabricating a VM" "1" "$rc"
[[ "$out" != *"cold"* && "$out" != *"warm"* ]] \
    && ok "outage: acquire printed no handle at all" \
    || bad "outage: acquire printed no handle at all" "$out"
mail_calls="$(grep -c '^mail:' "$MAIL_LOG" 2>/dev/null || true)"
is "outage: alarmed exactly once across 3 failed attempts" "1" "${mail_calls:-0}"
want "outage: alarm names the reason" "API unreachable" "$(cat "$MAIL_LOG")"

# A later acquire in the SAME outage (still failing, same reason) does not alarm again.
printf '3\n' > "$FAKE_DIR/fail-remaining"
: > "$MAIL_LOG"
out="$(run_rvm "SPIRA_ROUND_VM_STATE_DIR=$OUTAGE_STATE" acquire)"
mail_calls="$(grep -c '^mail:' "$MAIL_LOG" 2>/dev/null || true)"
is "outage: a second call in the same outage does not re-alarm" "0" "${mail_calls:-0}"

# ---------------------------------------------------------------------------
# 3 & 4. acquire with none records cold; acquire with a ready VM returns it warm and
#    starts exactly one background provision.
# ---------------------------------------------------------------------------
reset_fake
rm -rf "$POOL_STATE"
out="$(run_rvm_pool acquire)"
want "cold: acquire with nothing ready records cold" "cold" "$out"
first_handle="$(awk '{print $1}' <<<"$out")"

# Wait for the background provision (started by the cold acquire above) to land a new
# ready VM — proves acquire backgrounds the NEXT provision even on the cold path.
deadline=$(( $(date +%s) + 10 ))
while [ ! -r "$POOL_STATE/ready" ] && [ "$(date +%s)" -lt "$deadline" ]; do sleep 0.2; done
[ -r "$POOL_STATE/ready" ] && ok "cold: a background provision left a warm spare" \
    || bad "cold: a background provision left a warm spare" "no $POOL_STATE/ready after 10s"

provision_calls_after_cold="$(grep -c '^provision$' "$FAKE_DIR/calls.log" 2>/dev/null || true)"
is "cold: exactly one background provision started (plus the cold one = 2 total)" "2" "${provision_calls_after_cold:-0}"

out2="$(run_rvm_pool acquire)"
want "warm: the second acquire returns the spare, not a fresh cold provision" "warm" "$out2"
second_handle="$(awk '{print $1}' <<<"$out2")"
[ "$second_handle" != "$first_handle" ] \
    && ok "warm: the handed-out handle is the pre-provisioned spare, not the cold one" \
    || bad "warm: the handed-out handle is the pre-provisioned spare, not the cold one" \
        "same handle twice: $first_handle"

deadline=$(( $(date +%s) + 10 ))
while [ "$(grep -c '^provision$' "$FAKE_DIR/calls.log" 2>/dev/null || echo 0)" -lt 3 ] \
      && [ "$(date +%s)" -lt "$deadline" ]; do sleep 0.2; done
provision_calls_after_warm="$(grep -c '^provision$' "$FAKE_DIR/calls.log" 2>/dev/null || true)"
is "warm: exactly one MORE background provision started (3 total)" "3" "${provision_calls_after_warm:-0}"

# ---------------------------------------------------------------------------
# 5. Every acquire hands out a freshly provisioned VM. Nothing is ever reused, so a file
#    left on a released VM is never visible to the next round: there is no "next round"
#    that gets the same VM at all.
# ---------------------------------------------------------------------------
reset_fake
rm -rf "$POOL_STATE"
run_rvm_pool acquire >/dev/null   # cold vm-1 handed out, vm-2 backgrounded
run_rvm_pool release vm-1 >/dev/null 2>&1
[ -e "$FAKE_DIR/alive.vm-1" ] \
    && bad "release: destroy actually removed the VM's own state" "alive.vm-1 still present" \
    || ok "release: destroy actually removed the VM's own state"

deadline=$(( $(date +%s) + 10 ))
while [ ! -r "$POOL_STATE/ready" ] && [ "$(date +%s)" -lt "$deadline" ]; do sleep 0.2; done
out3="$(run_rvm_pool acquire)"
handle_b="$(awk '{print $1}' <<<"$out3")"
[ "$handle_b" != "vm-1" ] \
    && ok "leftover: the next round never receives the destroyed VM" \
    || bad "leftover: the next round never receives the destroyed VM" "handed out vm-1 again"

# ---------------------------------------------------------------------------
# 6. The manifest step refuses binaries whose tree sha differs from the one expected.
# ---------------------------------------------------------------------------
# A subprocess, isolated the same way every other call in this suite is (env -i, no real
# SPIRA_TOML/SPIRA_HOME): sourcing round-vm.sh in-process would instead run ITS conf.sh
# against this suite's own ambient environment. Sourcing (never executing) it this way
# defines _rvm_install_bins without running the dispatch at the bottom of the file — that
# block only fires when the file is the process itself (BASH_SOURCE[0] == $0), false here.
call_install_bins() {
    env -i PATH="$PATH" HOME="$TMP" SPIRA_HOME="$FAKE_HOME" \
        SPIRA_TOML="$TMP/no-such.toml" SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_RUN="$TMP/install-bins-run" \
        bash -c 'source "$0/round-vm.sh"; _rvm_install_bins "$1" "$2" "$3"' \
        "$HERE" "$1" "$2" "$3"
}

PULLED="$TMP/pulled-bins"
TARGET="$TMP/bins-target"

rm -rf "$PULLED" "$TARGET"; mkdir -p "$PULLED/wrongsha/release"
: > "$PULLED/wrongsha/release/fakebin"
if call_install_bins "$PULLED" "expectedsha" "$TARGET" 2>"$TMP/refuse.err"; then
    bad "manifest: mismatched tree sha is refused" "install returned success"
else
    ok "manifest: mismatched tree sha is refused"
fi
want "manifest: the refusal names both shas" "wrongsha" "$(cat "$TMP/refuse.err")"
want "manifest: the refusal names both shas" "expectedsha" "$(cat "$TMP/refuse.err")"
[ ! -e "$TARGET/expectedsha" ] \
    && ok "manifest: nothing installed when the tree sha does not match" \
    || bad "manifest: nothing installed when the tree sha does not match" "$TARGET/expectedsha exists"

rm -rf "$PULLED" "$TARGET"; mkdir -p "$PULLED/expectedsha/release"
: > "$PULLED/expectedsha/release/fakebin"
if call_install_bins "$PULLED" "expectedsha" "$TARGET"; then
    ok "manifest: a matching tree sha is installed"
else
    bad "manifest: a matching tree sha is installed" "install returned failure"
fi
[ -e "$TARGET/expectedsha/release/fakebin" ] \
    && ok "manifest: the binary lands exactly where land-local's own bins_dir computation reads it" \
    || bad "manifest: the binary lands exactly where land-local's own bins_dir computation reads it" \
        "missing $TARGET/expectedsha/release/fakebin"

# Absence of any pulled directory at all (a batch that never reached --with-bins) is not a
# refusal — there is nothing to refuse.
rm -rf "$PULLED" "$TARGET"
if call_install_bins "$PULLED" "expectedsha" "$TARGET"; then
    ok "manifest: no pulled bins at all is not treated as a refusal"
else
    bad "manifest: no pulled bins at all is not treated as a refusal" "returned failure"
fi

# ---------------------------------------------------------------------------
# 7. Round result measurement fields (sp-o3o6z): the pulled results directory may be flat (a
#    fixture testenv-batch.sh) or nested one level under testenv-batch.sh's own BATCH_KEY
#    cache-key directory — _rvm_results_leaf finds whichever one actually holds .result
#    files; _rvm_suite_wall_sum sums every suite's own wall time (not the phase's wall-clock
#    span); _rvm_read_build_wall reads testenv-batch.sh's own build_wall_s from runner.meta,
#    absent when --with-bins never built.
# ---------------------------------------------------------------------------
call_measure() {   # call_measure <fn> <args...>
    local fn="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" SPIRA_HOME="$FAKE_HOME" \
        SPIRA_TOML="$TMP/no-such.toml" SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_RUN="$TMP/measure-run" \
        bash -c 'source "$0/round-vm.sh"; "$1" "${@:2}"' \
        "$HERE" "$fn" "$@"
}

FLAT="$TMP/flat-results"; rm -rf "$FLAT"; mkdir -p "$FLAT"
printf 'ok %s 3 - serial explicit 0\n' "$(date +%s)" > "$FLAT/test-a.sh.result"
printf 'ok %s 4 - serial explicit 0\n' "$(date +%s)" > "$FLAT/test-b.sh.result"
is "measure: flat results dir is its own leaf" "$FLAT" "$(call_measure _rvm_results_leaf "$FLAT")"

# Nested one level, the shape testenv-batch.sh's own BATCH_KEY caching produces.
NESTED_ROOT="$TMP/nested-results"; rm -rf "$NESTED_ROOT"; mkdir -p "$NESTED_ROOT/deadbeef"
printf 'ok %s 5 - serial explicit 0\n' "$(date +%s)" > "$NESTED_ROOT/deadbeef/test-c.sh.result"
is "measure: nested results resolve to the BATCH_KEY subdirectory" "$NESTED_ROOT/deadbeef" \
    "$(call_measure _rvm_results_leaf "$NESTED_ROOT")"

# POSITIVE CONTROL: an empty directory has no leaf to find at all.
EMPTY_RESULTS="$TMP/empty-results"; rm -rf "$EMPTY_RESULTS"; mkdir -p "$EMPTY_RESULTS"
is "measure: an empty pulled dir has no leaf" "" "$(call_measure _rvm_results_leaf "$EMPTY_RESULTS")"

is "measure: sum of suite walls adds every suite's own wall time (3+4)" "7" "$(call_measure _rvm_suite_wall_sum "$FLAT")"
is "measure: sum of suite walls is 0 for an empty results dir" "0" "$(call_measure _rvm_suite_wall_sum "$EMPTY_RESULTS")"

printf 'nproc=8\nmemtotal_kb=100\nmaxpar=16\ncpu_busy_pct=50\nsuites_wall_s=7\nbuild_wall_s=42\n' > "$FLAT/runner.meta"
is "measure: build wall is read from runner.meta when --with-bins built" "42" "$(call_measure _rvm_read_build_wall "$FLAT")"
is "measure: build wall is empty when runner.meta has no build_wall_s (never --with-bins)" "" \
    "$(call_measure _rvm_read_build_wall "$EMPTY_RESULTS")"

tl_summary
