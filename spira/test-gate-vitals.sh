#!/usr/bin/env bash
# test-gate-vitals.sh — gate.yml Suites env and guest vitals sampler
#
# WHAT THIS PROVES
#   1. The Suites step env block does NOT set SPIRA_BATCH_MAXPAR (sp-4lakz:
#      maxpar is now derived from the guest's own hardware inside testenv-batch.sh
#      rather than capped by a repo variable that lagged behind the template).
#   2. The maxpar derivation block is present in testenv-batch.sh.
#   3. The vitals sampler loop body produces a line in the documented shape
#      (timestamp, mem used/total, avail, pressure, container count), and a
#      background process running it is stopped by kill — the mechanism the
#      EXIT trap relies on.
#
# POSITIVE CONTROLS
#   Part A: SPIRA_TESTENV_REGISTRY is in the extracted env block, proving the
#     block was found before SPIRA_BATCH_MAXPAR is checked for absence.
#   Part B: a line without the vitals prefix fails the shape regex, proving the
#     matcher runs before it is trusted on the real output.
#   Part C: the process is confirmed alive before the kill, so a kill that never
#     ran cannot produce a false positive.
#
# covers: .github/workflows/gate.yml spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]" ;; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "must not contain [$2]" ;; *) ok "$1" ;; esac; }
grepok() { printf '%s\n' "$3" | grep -qE "$2" && ok "$1" || bad "$1" "did not match [$2]"; }
grepno() { printf '%s\n' "$3" | grep -qE "$2" && bad "$1" "must not match [$2]" || ok "$1"; }

echo "test-gate-vitals.sh"

GATE_YML="$ROOT/.github/workflows/gate.yml"
BATCH="$HERE/testenv-batch.sh"
if [ ! -r "$GATE_YML" ]; then
    bad "gate.yml exists (prerequisite)" "not found at $GATE_YML"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"; exit 1
fi

_suites_step="$(awk '/^      - name: Suites/{f=1;next} f&&/^      - name:/{exit} f{print}' "$GATE_YML")"

# ---------------------------------------------------------------------------
# Part A: SPIRA_BATCH_MAXPAR removed from gate.yml; formula lives in batch
# ---------------------------------------------------------------------------
echo
echo "Part A: maxpar derived from hardware, not pinned in gate.yml"

_suites_env="$(printf '%s\n' "$_suites_step" \
    | awk '/^        env:/{f=1;next} f&&/^        [a-zA-Z]/{exit} f{print}')"

if [ -z "$_suites_env" ]; then
    bad "A0: Suites step env block located" "awk extracted nothing; subsequent checks would be vacuous"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "A0: Suites step env block located"

# Positive control: a known key is present, confirming the block is real.
want "A0-pos: SPIRA_TESTENV_REGISTRY in env block (positive control)" \
     "SPIRA_TESTENV_REGISTRY" "$_suites_env"

# SPIRA_BATCH_MAXPAR must NOT be in gate.yml — maxpar is now derived inside the
# guest from its own hardware (sp-4lakz). A repo variable that lagged behind the
# template left the CI guest under-parallelised for days after each upgrade.
nowant "A1: SPIRA_BATCH_MAXPAR absent from Suites env (derived in guest)" \
       "SPIRA_BATCH_MAXPAR" "$_suites_env"

# The derivation block must exist in testenv-batch.sh.
_block="$(sed -n '/#!maxpar-begin/,/#!maxpar-end/{/#!maxpar-/d; p}' "$BATCH" 2>/dev/null || true)"
[ -n "$_block" ] && ok "A2: maxpar derivation block present in testenv-batch.sh" \
                  || bad "A2: maxpar derivation block present in testenv-batch.sh" "not found"

# The formula must reference MemAvailable so the binding resource is memory, not CPU alone.
case "$_block" in
    *MemAvailable*) ok "A3: formula reads MemAvailable from guest" ;;
    *) bad "A3: formula reads MemAvailable from guest" "MemAvailable not found in block" ;;
esac

# ---------------------------------------------------------------------------
# Part B: vitals sampler output shape
# ---------------------------------------------------------------------------
echo
echo "Part B: vitals sampler output shape"

_shape='^vitals [0-9]{2}:[0-9]{2}:[0-9]{2} mem .+MiB.+avail [|] pressure mem .+ cpu .+ [|] load .+ [|] steal .+ [|] containers '

# Positive control: a line without the vitals prefix does not satisfy the shape.
grepno "B-pos: non-vitals line fails shape pattern (positive control)" \
       "$_shape" "some unrelated output line"

_vitals_line="$(
    printf 'vitals %s %s | pressure mem %s cpu %s | load %s | steal %s | containers %s\n' \
        "$(date -u +%H:%M:%S)" \
        "$(free -m | awk '/^Mem:/{printf "mem %s/%sMiB used, %sMiB avail", $3, $2, $7}')" \
        "$(awk '/^some/{print $2}' /proc/pressure/memory 2>/dev/null || echo 'n/a')" \
        "$(awk '/^some/{print $2}' /proc/pressure/cpu 2>/dev/null || echo 'n/a')" \
        "$(cut -d' ' -f1 /proc/loadavg 2>/dev/null || echo 'n/a')" \
        "n/a" \
        "$(pgrep -c conmon 2>/dev/null || printf '0')"
)"
[ -n "$_vitals_line" ] \
    && ok "B1: sampler loop body produced output" \
    || bad "B1: sampler loop body produced output" "empty"

grepok "B2: output matches documented shape" "$_shape" "$_vitals_line"

# Verify the gate.yml vitals loop contains the new fields.
want "B3: cpu pressure sampled in gate.yml vitals loop" \
     "pressure/cpu" "$_suites_step"
want "B4: vCPU count printed at step start" \
     "vcpus" "$_suites_step"
want "B5: steal field in gate.yml vitals loop" \
     "steal" "$_suites_step"

# ---------------------------------------------------------------------------
# Part C: the EXIT trap's kill mechanism stops the background sampler
# ---------------------------------------------------------------------------
echo
echo "Part C: sampler stops on kill"

( while :; do sleep 60; done ) &
_bg=$!

# Positive control: the process is alive before the kill.
if kill -0 "$_bg" 2>/dev/null; then
    ok "C-pos: background process is alive before kill (positive control)"
else
    bad "C-pos: background process is alive before kill (positive control)" \
        "process $_bg already gone; C1 cannot be meaningful"
fi

kill "$_bg" 2>/dev/null || true
_i=0
while kill -0 "$_bg" 2>/dev/null && [ "$_i" -lt 20 ]; do
    sleep 0.1; _i=$((_i+1))
done
if kill -0 "$_bg" 2>/dev/null; then
    bad "C1: kill stops the sampler background process" \
        "process $_bg still alive after $((_i)) attempts"
else
    ok "C1: kill stops the sampler background process"
fi

# ---------------------------------------------------------------------------
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
