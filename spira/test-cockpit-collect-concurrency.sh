#!/usr/bin/env bash
#
# test-cockpit-collect-concurrency.sh — collect.sh's slow-tier concurrency cap and probe
# registry shape.
#
# Before this suite, gap #9 of docs/test-plan/cockpit-observability.md: neither had any
# coverage.
#
#   1. SLOW_CONCURRENT (the comment at collect.sh:76): a slow-tier probe (interval>=600)
#      already running holds its concurrency slot, so a second due slow probe is skipped —
#      not started — until the first's process is gone. Runs the real scheduling code
#      collect.sh's supervisor loop executes each tick, extracted at runtime (not copied),
#      the same technique test-cockpit-collector-watchdog.sh uses for its function.
#   2. PROBE REGISTRY SHAPE: all 16 entries are well-formed (4 colon-separated fields, an
#      interval matching one of the three declared tiers, a positive timeout, no duplicate
#      probe names), and at least one real entry has a timeout SHORTER than its interval —
#      the case the gap names explicitly. Both matter: PROBE_PID and PROBE_LAST are keyed
#      by name, so a duplicate silently drops a probe from scheduling.
#
# tier: T1
# covers: spira/collect.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COLLECT="$HERE/collect.sh"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ============================================================================
echo "1. probe registry: shape and the timeout-shorter-than-interval case"
# ============================================================================

# The array literal itself, extracted the same way — not retyped, so this suite tracks
# collect.sh's actual table rather than a copy of today's numbers.
PROBES_SRC="$(awk '/^PROBES=\(/{p=1} p{print} p && /^\)$/{exit}' "$COLLECT")"
[ -n "$PROBES_SRC" ] || { echo "SKIP: PROBES array not found in collect.sh" >&2; exit 1; }
eval "$PROBES_SRC"

dup=0; malformed=0; bad_interval=0; bad_timeout=0; shorter_timeout=0
declare -A seen_names=()
for entry in "${PROBES[@]}"; do
    IFS=: read -r name interval timeout_s cmd rest <<<"$entry"
    if [ -z "$name" ] || [ -z "$interval" ] || [ -z "$timeout_s" ] || [ -z "$cmd" ] || [ -n "${rest:-}" ]; then
        malformed=$((malformed+1))
    fi
    if [ -n "${seen_names[$name]:-}" ]; then dup=$((dup+1)); fi
    seen_names[$name]=1
    case "$interval" in 5|60|600) ;; *) bad_interval=$((bad_interval+1)) ;; esac
    [ "${timeout_s:-0}" -gt 0 ] 2>/dev/null || bad_timeout=$((bad_timeout+1))
    [ "${timeout_s:-0}" -lt "${interval:-0}" ] 2>/dev/null && shorter_timeout=$((shorter_timeout+1))
done

is "16 probes registered"                       "16" "${#PROBES[@]}"
is "no malformed entries (4 colon-fields each)" "0"  "$malformed"
is "no duplicate probe names"                   "0"  "$dup"
is "every interval is a declared tier (5/60/600)" "0" "$bad_interval"
is "every timeout is a positive integer"        "0"  "$bad_timeout"
if [ "$shorter_timeout" -gt 0 ]; then
    ok "at least one entry has timeout < interval (the gap's own example)"
else
    bad "at least one entry has timeout < interval (the gap's own example)" "found none"
fi

# ============================================================================
echo ""
echo "2. SLOW_CONCURRENT: a running slow probe blocks a second due slow probe"
# ============================================================================

# The exact scheduling block collect.sh's supervisor loop runs each tick, extracted between
# two markers unique to it (not a whole function, since it lives inline in _supervisor_loop's
# while body — same reasoning as extracting cockpit.sh functions, applied to a block instead).
START_LN="$(grep -n '^        local slow_running=0' "$COLLECT" | head -1 | cut -d: -f1)"
END_LN="$(grep -n 'if _merge_fragments; then' "$COLLECT" | head -1 | cut -d: -f1)"
[ -n "$START_LN" ] && [ -n "$END_LN" ] || { echo "SKIP: scheduling block markers not found" >&2; exit 1; }
SCHED_BLOCK="$(sed -n "${START_LN},$((END_LN-1))p" "$COLLECT")"

RUN_BODY="$(awk '/^_run_probe_body\(\)/{p=1} p{print} p && /^\}$/{exit}' "$COLLECT")"
[ -n "$RUN_BODY" ] || { echo "SKIP: _run_probe_body not found in collect.sh" >&2; exit 1; }

FRAG_DIR="$TMP/cockpit.d"; mkdir -p "$FRAG_DIR"
COCK="$TMP/mock-cockpit.sh"
cat > "$COCK" <<'SH'
#!/usr/bin/env bash
echo "SP_MOCK=1"
SH
chmod +x "$COCK"

RESULT="$TMP/result.env"
bash <<DRIVER > "$RESULT" 2>&1
set -uo pipefail
FRAG_DIR="$FRAG_DIR"
COCK="$COCK"
$RUN_BODY

# The extracted block uses \`local\`, valid only inside a function — it runs as one
# statement of _supervisor_loop's own while-body in the real code.
run_schedule_pass() {
$SCHED_BLOCK
}

PROBES=("slow_a:600:300:mock" "slow_b:600:300:mock")
SLOW_CONCURRENT=1
declare -A PROBE_PID=()
declare -A PROBE_LAST=()
_now=\$(date +%s)

# PASS 1: slow_a is already running (a real, long-lived process holding its slot) from an
# earlier tick — PROBE_LAST[slow_a] is recent, so it is not itself due. slow_b IS due
# (PROBE_LAST[slow_b] left at 0). The cap must block slow_b from starting this pass.
sleep 100 &
PROBE_PID[slow_a]=\$!
PROBE_LAST[slow_a]=\$_now

run_schedule_pass

echo "PASS1_B_PID=\${PROBE_PID[slow_b]:-}"

# PASS 2: slow_a's process is gone (its pass finished) — its slot is free. slow_b is still
# due (PROBE_LAST[slow_b] untouched by pass 1). It must start now.
kill \${PROBE_PID[slow_a]} 2>/dev/null
wait \${PROBE_PID[slow_a]} 2>/dev/null

_now=\$(date +%s)
run_schedule_pass
wait \${PROBE_PID[slow_b]:-} 2>/dev/null

echo "PASS2_B_PID=\${PROBE_PID[slow_b]:-}"
DRIVER

pass1_pid="$(grep '^PASS1_B_PID=' "$RESULT" | sed 's/^PASS1_B_PID=//')"
pass2_pid="$(grep '^PASS2_B_PID=' "$RESULT" | sed 's/^PASS2_B_PID=//')"

if [ -z "$pass1_pid" ]; then
    ok "pass 1: slow_b did NOT start while slow_a holds the only slot"
else
    bad "pass 1: slow_b did NOT start while slow_a holds the only slot" "started as pid $pass1_pid"
fi
if [ -n "$pass2_pid" ]; then
    ok "pass 2: slow_b starts once slow_a's slot frees up"
else
    bad "pass 2: slow_b starts once slow_a's slot frees up" "still did not start"
fi
if [ -f "$FRAG_DIR/slow_b.env" ] && grep -q '^SP_MOCK=1' "$FRAG_DIR/slow_b.env"; then
    ok "slow_b's fragment carries the real probe's output"
else
    bad "slow_b's fragment carries the real probe's output" "$(cat "$FRAG_DIR/slow_b.env" 2>&1)"
fi

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
