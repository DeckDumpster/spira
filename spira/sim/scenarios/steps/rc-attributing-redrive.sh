#!/bin/sh
# rc-attributing-redrive.sh <bead> — drive one round of <bead> by hand, as a re-certify does: pass 1
# ends red at lint before any suite ran, pass 2 starts straight from ATTRIBUTING and goes green, and
# the round lands. Exit 3 until <bead> is CERTIFIED, 0 once it landed with a clean batch history,
# 9 on the first assertion that does not hold.
set -u
bead="$1"
fail() {
    echo "rc-attributing-redrive: $1" >&2
    [ -n "${batch:-}" ] && spira-lc history "$batch" --machine batch 2>/dev/null | grep -o '"event": "[A-Za-z]*"\|"refusal": "[A-Za-z]*"' | paste -sd" " - | sed "s|^|rc-attributing-redrive: history: |" >&2
    exit 9
}
state=$(spira-lc state "$bead" 2>/dev/null)
[ "$state" = LANDED ] && exit 0
[ "$state" = CERTIFIED ] || exit 3

out=$(queue round open sim --members "$bead" 2>&1) || fail "round open: $out"
batch=$(queue round status sim | sed -n 's/^batch_id=//p' | head -1)
[ -n "$batch" ] || fail "round open left no open round"

echo "red-early spira-lint" > "$SIM_WORLD/gate-verdict"
out=$(queue round certify "$batch" sim 2>&1); rc=$?
[ "$rc" = 1 ] || fail "pass 1 exited $rc, not 1 (red): $out"
printf '%s\n' "$out" | grep -qx 'red=spira-lint' || fail "pass 1 was not judged red on spira-lint: $out"

echo green > "$SIM_WORLD/gate-verdict"
out=$(queue round certify "$batch" sim 2>&1) || fail "pass 2 was not green: $out"

spira-lc history "$batch" --machine batch > "$SIM_WORLD/redrive.history" || fail "no history for $batch"
python3 - "$SIM_WORLD/redrive.history" <<'PY' || exit 9
import json, sys
rows = json.load(open(sys.argv[1]))
def ev(r):
    e = r.get("evidence")
    return json.loads(e) if isinstance(e, str) else (e or {})
def applied(r):
    return str(r.get("applied")) in ("1", "True", "true")
bad = [r for r in rows if r.get("refusal") == "IllegalTransition"]
if bad:
    sys.exit("rc-attributing-redrive: refused as illegal: %s" % [(r["event"], r["from_state"]) for r in bad])
def has(event, **want):
    return any(applied(r) and r.get("event") == event and all(ev(r).get(event, {}).get(k) == v for k, v in want.items()) for r in rows)
if not has("PassRed", n=1, red_suites=["spira-lint"]):
    sys.exit("rc-attributing-redrive: pass 1 is not PassRed naming spira-lint: %s" % [r["event"] for r in rows])
if has("PassIncomplete", n=1):
    sys.exit("rc-attributing-redrive: pass 1 was recorded incomplete")
if not has("PassStarted", n=2):
    sys.exit("rc-attributing-redrive: pass 2 never started")
if any(applied(r) and r.get("event") == "PassRebuilt" for r in rows):
    sys.exit("rc-attributing-redrive: pass 2 went through PassRebuilt, not straight from ATTRIBUTING")
if not has("PassGreen", n=2):
    sys.exit("rc-attributing-redrive: pass 2 is not green")
PY

out=$(queue round land "$batch" sim 2>&1) || fail "the green round did not land: $out"
