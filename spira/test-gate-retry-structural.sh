#!/usr/bin/env bash
# test-gate-retry-structural.sh — structural-failure heuristic skips the serial re-run
# covers: spira/gate-retry.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "want [$2] got [$3]"; }
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "no [$3] in [$2]" ;; esac; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/batch" <<'B'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$STUB_LOG.args"
cat > "$STUB_LOG.stdin"
mkdir -p "$SPIRA_BATCH_RESULTS/k"
while read -r s; do [ -n "$s" ] && printf '%s 1 1 - serial explicit\n' "$STUB_STATUS" > "$SPIRA_BATCH_RESULTS/k/$s.result"; done < "$STUB_LOG.stdin"
exit "$STUB_RC"
B
chmod +x "$TMP/batch"

first() {
    local d="$1"; shift; rm -rf "$d" "$d-retry"; mkdir -p "$d/key"
    for kv in "$@"; do printf '%s 1 1 - parallel explicit\n' "${kv#*=}" > "$d/key/${kv%%=*}.result"; done
}

echo "test-gate-retry-structural.sh"

# 8 reds out of 10 — structural path: exits non-zero, prints message, emits annotations, no batch
rm -f "$TMP/log.args"
first "$TMP/r" \
    a.sh=red b.sh=red c.sh=red d.sh=red e.sh=red f.sh=red g.sh=red h.sh=red \
    i.sh=ok j.sh=ok
GATE_RETRY_BATCH="$TMP/batch" STUB_LOG="$TMP/log" STUB_STATUS="red" STUB_RC="1" \
    bash "$HERE/gate-retry.sh" "$TMP/r" deadbeef > "$TMP/out" 2>&1; rc=$?
is  "8 of 10 reds: exits non-zero"                 "1" "$rc"
has "prints structural line"                        "$(cat "$TMP/out")" "structural, not flaky"
has "names the red and total count"                 "$(cat "$TMP/out")" "8 of 10"
has "emits error annotation for a red suite"        "$(cat "$TMP/out")" "::error title=red suite::a.sh"
is  "retry root is not created"                     "no" "$([ -d "$TMP/r-retry" ] && echo yes || echo no)"
is  "batch is never invoked"                        "no" "$([ -f "$TMP/log.args" ] && echo yes || echo no)"

# 2 reds out of 10 — below both thresholds, serial re-run proceeds
rm -f "$TMP/log.args"
first "$TMP/r" \
    a.sh=red b.sh=red \
    c.sh=ok d.sh=ok e.sh=ok f.sh=ok g.sh=ok h.sh=ok i.sh=ok j.sh=ok
GATE_RETRY_BATCH="$TMP/batch" STUB_LOG="$TMP/log" STUB_STATUS="ok" STUB_RC="0" \
    bash "$HERE/gate-retry.sh" "$TMP/r" deadbeef > "$TMP/out" 2>&1
is  "2 of 10 reds: batch is invoked"               "yes" "$([ -f "$TMP/log.args" ] && echo yes || echo no)"
has "in serial mode"                               "$(cat "$TMP/log.args")" "--mode serial"
has "passing the 2 red suite names"                "$(cat "$TMP/log.stdin")" "a.sh"

# GATE_RETRY_MAX_RETRY=20 with 8 reds out of 20 total — raised threshold, neither condition fires
# (8 <= 20 and 8 is not more than half of 20), so serial re-run proceeds
rm -f "$TMP/log.args"
first "$TMP/r" \
    a.sh=red b.sh=red c.sh=red d.sh=red e.sh=red f.sh=red g.sh=red h.sh=red \
    i.sh=ok j.sh=ok k.sh=ok l.sh=ok m.sh=ok n.sh=ok o.sh=ok p.sh=ok q.sh=ok r.sh=ok s.sh=ok t.sh=ok
GATE_RETRY_MAX_RETRY=20 GATE_RETRY_BATCH="$TMP/batch" STUB_LOG="$TMP/log" STUB_STATUS="ok" STUB_RC="0" \
    bash "$HERE/gate-retry.sh" "$TMP/r" deadbeef > "$TMP/out" 2>&1
is  "GATE_RETRY_MAX_RETRY=20 with 8 of 20 reds: batch invoked" "yes" "$([ -f "$TMP/log.args" ] && echo yes || echo no)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
