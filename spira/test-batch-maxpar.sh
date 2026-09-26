#!/usr/bin/env bash
# test-batch-maxpar.sh — maxpar derived from guest hardware when SPIRA_BATCH_MAXPAR unset
#
# WHAT THIS PROVES
#   1. testenv-batch.sh derives maxpar from min(cpu_ceiling, floor((MemAvailable-reserve)/per_suite))
#      rather than a literal. Memory is the binding resource in the guest; the formula caps
#      the pool at the memory-bound value when MemAvailable is low.
#   2. When cpu_ceiling <= memory-bound, CPU is the binding input (and vice versa).
#   3. cpu_ceiling defaults to nproc but SPIRA_BATCH_MAXPAR_CEILING can raise it past nproc,
#      with the memory term still enforced — concurrency is not welded to core count.
#   4. SPIRA_BATCH_MAXPAR, SPIRA_BATCH_MAXPAR_CEILING, SPIRA_BATCH_MEM_RESERVE_MIB,
#      SPIRA_BATCH_MEM_PER_SUITE_MIB, SPIRA_BATCH_MEM_AVAIL_MIB, and SPIRA_BATCH_PSI_THRESHOLD
#      are in SPIRA_CONF_KEYS.
#
# POSITIVE CONTROLS (law-a-regression-test-must-be-seen-to-fail)
#   Part A plants fake nproc (16) and fake MemAvailable (11GiB then 3GiB). The 11GiB case
#   expects cpu-bound at 16; the 3GiB case expects memory-bound at 4. If the formula
#   reverts to a literal the cpu/memory-binding label disappears and A2b/A3b fail.
#   A9/A10 plant SPIRA_BATCH_MAXPAR_CEILING above the fake nproc: if the ceiling were still
#   welded to nproc, maxpar could never exceed it and A9 would fail.
#   Part B's positive control asserts a fabricated key is absent from SPIRA_CONF_KEYS.
#
# covers: spira/testenv-batch.sh spira/conf.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

. "$HERE/testlib.sh"

BATCH="$HERE/testenv-batch.sh"
CONF_SH="$HERE/conf.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-batch-maxpar.sh"

# ===========================================================================
# PART A: maxpar derived from hardware (cpu-bound and memory-bound cases)
# ===========================================================================
echo
echo "Part A: maxpar formula — cpu-bound and memory-bound"

# Extract the derivation block from the batch script.
_block="$(sed -n '/#!maxpar-begin/,/#!maxpar-end/{/#!maxpar-/d; p}' "$BATCH")"
[ -n "$_block" ] || {
    printf '  FAIL  A0: maxpar derivation block not found in %s\n' "$BATCH" >&2
    exit 1
}
ok "A0: maxpar derivation block found"

FAKE_BIN="$TMP/bin"
mkdir -p "$FAKE_BIN"
printf '#!/bin/sh\necho 16\n' > "$FAKE_BIN/nproc"
chmod +x "$FAKE_BIN/nproc"

# Case 1: MemAvailable=11GiB (11264MiB), reserve=1024, per_suite=512 → mem_bound=20 → cpu wins → maxpar=16
_eval_maxpar() {
    PATH="$FAKE_BIN:$PATH" \
    SPIRA_BATCH_MEM_AVAIL_MIB="$1" \
    SPIRA_BATCH_MEM_RESERVE_MIB=1024 \
    SPIRA_BATCH_MEM_PER_SUITE_MIB=512 \
    bash -c "unset SPIRA_BATCH_MAXPAR 2>/dev/null||true; $_block; printf '%s %s' \"\$_maxpar\" \"\$_maxpar_binding\""
}

_r1="$(_eval_maxpar 11264)"
_maxpar1="${_r1%% *}"
_binding1="${_r1##* }"

[ "$_maxpar1" = "16" ] && ok "A1: 11GiB avail → maxpar 16" \
                        || bad "A1: 11GiB avail → maxpar 16" "got '$_maxpar1'"
[ "$_binding1" = "cpu" ] && ok "A2: 11GiB avail → cpu-bound" \
                          || bad "A2: 11GiB avail → cpu-bound" "got binding='$_binding1'"

# Case 2: MemAvailable=3GiB (3072MiB), reserve=1024, per_suite=512 → mem_bound=4 → memory wins → maxpar=4
_r2="$(_eval_maxpar 3072)"
_maxpar2="${_r2%% *}"
_binding2="${_r2##* }"

[ "$_maxpar2" = "4" ] && ok "A3: 3GiB avail → maxpar 4" \
                       || bad "A3: 3GiB avail → maxpar 4" "got '$_maxpar2'"
[ "$_binding2" = "memory" ] && ok "A4: 3GiB avail → memory-bound" \
                             || bad "A4: 3GiB avail → memory-bound" "got binding='$_binding2'"

# Case 3: SPIRA_BATCH_MAXPAR=2 (ceiling), nproc=16, 11GiB avail (hardware=cpu-bound 16) → override=2
_r_ov="$(
    PATH="$FAKE_BIN:$PATH" \
    SPIRA_BATCH_MEM_AVAIL_MIB=11264 \
    SPIRA_BATCH_MEM_RESERVE_MIB=1024 \
    SPIRA_BATCH_MEM_PER_SUITE_MIB=512 \
    bash -c "SPIRA_BATCH_MAXPAR=2; $_block; printf '%s %s' \"\$_maxpar\" \"\$_maxpar_binding\""
)"
_maxpar_ov="${_r_ov%% *}"
_binding_ov="${_r_ov##* }"
[ "$_maxpar_ov" = "2" ] && ok "A5: SPIRA_BATCH_MAXPAR=2 ceiling below hardware=16 → maxpar 2" \
                         || bad "A5: SPIRA_BATCH_MAXPAR=2 ceiling below hardware=16 → maxpar 2" "got '$_maxpar_ov'"
[ "$_binding_ov" = "override" ] && ok "A6: binding=override when ceiling below hardware" \
                                  || bad "A6: binding=override when ceiling below hardware" "got '$_binding_ov'"

# Case 4: SPIRA_BATCH_MAXPAR=99 (above hardware-bound), nproc=16, 3GiB avail (hardware=memory-bound 4) → hardware wins
_r_cap="$(
    PATH="$FAKE_BIN:$PATH" \
    SPIRA_BATCH_MEM_AVAIL_MIB=3072 \
    SPIRA_BATCH_MEM_RESERVE_MIB=1024 \
    SPIRA_BATCH_MEM_PER_SUITE_MIB=512 \
    bash -c "SPIRA_BATCH_MAXPAR=99; $_block; printf '%s %s' \"\$_maxpar\" \"\$_maxpar_binding\""
)"
_maxpar_cap="${_r_cap%% *}"
_binding_cap="${_r_cap##* }"
[ "$_maxpar_cap" = "4" ] && ok "A7: SPIRA_BATCH_MAXPAR=99 above hardware-bound=4 → hardware wins at 4" \
                          || bad "A7: SPIRA_BATCH_MAXPAR=99 above hardware-bound=4 → hardware wins at 4" "got '$_maxpar_cap'"
[ "$_binding_cap" = "memory" ] && ok "A8: binding=memory when override above hardware" \
                                 || bad "A8: binding=memory when override above hardware" "got '$_binding_cap'"

# Case 5: SPIRA_BATCH_MAXPAR_CEILING=24 (above fake nproc=16), plenty of memory
# (avail=100000MiB, reserve=1024, per_suite=192 → mem_bound=515) → ceiling wins at 24,
# exceeding nproc. Proves concurrency is no longer welded to core count.
_r_ceil="$(
    PATH="$FAKE_BIN:$PATH" \
    SPIRA_BATCH_MAXPAR_CEILING=24 \
    SPIRA_BATCH_MEM_AVAIL_MIB=100000 \
    SPIRA_BATCH_MEM_RESERVE_MIB=1024 \
    SPIRA_BATCH_MEM_PER_SUITE_MIB=192 \
    bash -c "unset SPIRA_BATCH_MAXPAR 2>/dev/null||true; $_block; printf '%s %s' \"\$_maxpar\" \"\$_maxpar_binding\""
)"
_maxpar_ceil="${_r_ceil%% *}"
_binding_ceil="${_r_ceil##* }"
[ "$_maxpar_ceil" = "24" ] && ok "A9: SPIRA_BATCH_MAXPAR_CEILING=24 exceeds fake nproc=16 → maxpar 24" \
                           || bad "A9: SPIRA_BATCH_MAXPAR_CEILING=24 exceeds fake nproc=16 → maxpar 24" "got '$_maxpar_ceil'"
[ "$_binding_ceil" = "cpu" ] && ok "A10: binding=cpu when ceiling below memory-bound" \
                              || bad "A10: binding=cpu when ceiling below memory-bound" "got '$_binding_ceil'"

# Case 6: SPIRA_BATCH_MAXPAR_CEILING=64 (well above nproc), but mem_bound=4
# (avail=3072MiB, reserve=1024, per_suite=512) → memory still wins at 4, regardless of ceiling.
_r_ceilmem="$(
    PATH="$FAKE_BIN:$PATH" \
    SPIRA_BATCH_MAXPAR_CEILING=64 \
    SPIRA_BATCH_MEM_AVAIL_MIB=3072 \
    SPIRA_BATCH_MEM_RESERVE_MIB=1024 \
    SPIRA_BATCH_MEM_PER_SUITE_MIB=512 \
    bash -c "unset SPIRA_BATCH_MAXPAR 2>/dev/null||true; $_block; printf '%s %s' \"\$_maxpar\" \"\$_maxpar_binding\""
)"
_maxpar_ceilmem="${_r_ceilmem%% *}"
_binding_ceilmem="${_r_ceilmem##* }"
[ "$_maxpar_ceilmem" = "4" ] && ok "A11: SPIRA_BATCH_MAXPAR_CEILING=64 still memory-capped at 4" \
                             || bad "A11: SPIRA_BATCH_MAXPAR_CEILING=64 still memory-capped at 4" "got '$_maxpar_ceilmem'"
[ "$_binding_ceilmem" = "memory" ] && ok "A12: binding=memory when ceiling exceeds memory-bound" \
                                    || bad "A12: binding=memory when ceiling exceeds memory-bound" "got '$_binding_ceilmem'"

# ===========================================================================
# PART B: new keys are accepted by conf.sh's allowlist
# ===========================================================================
echo
echo "Part B: new conf keys in SPIRA_CONF_KEYS"

_conf_keys="$(
    SPIRA_HOME="$HERE" \
    SPIRA_CONF=/nonexistent \
    bash -c ". '$CONF_SH'; printf '%s' \"\$SPIRA_CONF_KEYS\""
)"

for _key in SPIRA_BATCH_MAXPAR SPIRA_BATCH_MAXPAR_CEILING SPIRA_BATCH_MEM_RESERVE_MIB SPIRA_BATCH_MEM_PER_SUITE_MIB \
            SPIRA_BATCH_MEM_AVAIL_MIB SPIRA_BATCH_PSI_THRESHOLD; do
    want "B-${_key}" "$_key" "$_conf_keys"
done

# Positive control: fabricated key is absent.
nowant "B-pos: SPIRA_BATCH_NOEXIST absent (positive control)" \
        "SPIRA_BATCH_NOEXIST" "$_conf_keys"

# Verify a value from a conf file is picked up for a new key.
CONF_FILE="$TMP/spira.conf"
printf 'SPIRA_BATCH_MEM_PER_SUITE_MIB=256\n' > "$CONF_FILE"
_from_conf="$(
    SPIRA_HOME="$HERE" \
    SPIRA_CONF="$CONF_FILE" \
    bash -c "unset SPIRA_BATCH_MEM_PER_SUITE_MIB; . '$CONF_SH'; printf '%s' \"\${SPIRA_BATCH_MEM_PER_SUITE_MIB:-unset}\""
)"
[ "$_from_conf" = "256" ] && ok "B-conf: spira.conf line sets SPIRA_BATCH_MEM_PER_SUITE_MIB" \
                           || bad "B-conf: spira.conf line sets SPIRA_BATCH_MEM_PER_SUITE_MIB" \
                                  "expected 256, got '$_from_conf'"

# ===========================================================================
echo
tl_summary
