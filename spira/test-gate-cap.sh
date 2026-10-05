#!/usr/bin/env bash
#
# test-gate-cap.sh — gate.sh's hard wall-clock cap (Ryan 2026-10-05: a gate is 300 s, whole;
# a round's gate once ran 2981 s to a red). A stub `gate` first on PATH stands in for the
# binary, so the cap is proven in seconds with SPIRA_GATE_CAP_SECS lowering it:
#   1. a gate that outruns the cap is killed, and the verdict is a named FAIL reason=over-cap;
#   2. positive control: a gate inside the cap passes its own output and exit code through;
#   3. a value above 300 is not honoured (the cap only ever goes down) — read from the script;
#   4. a TERM to gate.sh reaches the gate it started (killing gate.sh still kills the gate).
#
# tier: T1
# covers: spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-gate-cap.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
STUB="$T/bin"; mkdir -p "$STUB"
# The stub gate: sleeps $STUB_SLEEP, records its pid, prints a verdict, exits $STUB_RC.
cat > "$STUB/gate" <<'EOF'
#!/usr/bin/env bash
echo $$ > "$STUB_PIDFILE"
sleep "${STUB_SLEEP:-0}"
echo "gate: VERDICT=PASS reason=stub branch=b repo=r suite=-" >&2
exit "${STUB_RC:-0}"
EOF
chmod +x "$STUB/gate"
export PATH="$STUB:$PATH" STUB_PIDFILE="$T/gate.pid"

echo
echo "1. a gate past the cap is killed with a named over-cap verdict:"
t0=$SECONDS
out="$(SPIRA_GATE_CAP_SECS=2 STUB_SLEEP=60 bash "$HERE/gate.sh" br repo 2>&1)"; rc=$?
took=$((SECONDS - t0))
is   "over-cap: exit 1 (a FAIL, not a wait)" 1 "$rc"
want "over-cap: the verdict names the cap" "VERDICT=FAIL reason=over-cap branch=br repo=repo" "$out"
[ "$took" -le 15 ] && ok "over-cap: returned within cap + kill grace (${took}s)" || bad "over-cap: returned within cap + kill grace" "${took}s"
nowant "over-cap: the stub never reached its own verdict" "reason=stub" "$out"

echo
echo "2. positive control — a gate inside the cap passes through untouched:"
out="$(SPIRA_GATE_CAP_SECS=10 STUB_SLEEP=0 STUB_RC=3 bash "$HERE/gate.sh" br repo 2>&1)"; rc=$?
is   "inside the cap: the gate's own exit code" 3 "$rc"
want "inside the cap: the gate's own verdict" "VERDICT=PASS reason=stub" "$out"
nowant "inside the cap: no over-cap verdict" "over-cap" "$out"

echo
echo "3. the cap only goes down — a value above 300 leaves 300:"
want "gate.sh: the cap constant is 300" "GATE_CAP_SECS=300" "$(cat "$HERE/gate.sh")"
want "gate.sh: SPIRA_GATE_CAP_SECS is honoured only below 300" '-lt 300 ] && GATE_CAP_SECS="$SPIRA_GATE_CAP_SECS"' "$(cat "$HERE/gate.sh")"

echo
echo "4. a TERM to gate.sh reaches the gate:"
rm -f "$T/gate.pid"
SPIRA_GATE_CAP_SECS=60 STUB_SLEEP=60 bash "$HERE/gate.sh" br repo >/dev/null 2>&1 &
gs=$!
for _ in $(seq 1 100); do [ -s "$T/gate.pid" ] && break; sleep 0.1; done
gp="$(cat "$T/gate.pid" 2>/dev/null || true)"
[ -n "$gp" ] && kill -0 "$gp" 2>/dev/null && ok "fixture: the stub gate is running (pid $gp)" || bad "fixture: the stub gate is running" "no pid"
kill -TERM "$gs" 2>/dev/null
for _ in $(seq 1 50); do kill -0 "$gp" 2>/dev/null || break; sleep 0.1; done
kill -0 "$gp" 2>/dev/null && bad "TERM to gate.sh kills the gate" "pid $gp still alive" || ok "TERM to gate.sh kills the gate"
wait "$gs" 2>/dev/null

tl_summary
