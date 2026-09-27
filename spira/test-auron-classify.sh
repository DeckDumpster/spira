#!/usr/bin/env bash
#
# test-auron-classify.sh — the watchdog's classifier, as the pure function it is.
#
#   ./test-auron-classify.sh
#
# THE NEGATIVE CASES CARRY THE WEIGHT HERE, more than in any other suite. Auron's whole
# job is to be believed, and the expensive failure is not a missed stall — the loop is
# watched by a human too — but a false one, because a watchdog that cries during ordinary
# operation gets scrolled past, and then the one real alert lands in a pane that has been
# taught to ignore it (law-alerts-must-be-actionable).
#
# So two of the cases below are REPLAYS OF REAL SENTINEL LOG, committed under testdata/:
#
#   sentinel-healthy.log     ~90 real passes over three hours of ordinary operation.
#                            Nothing may fire over it. This is the noise floor, measured
#                            rather than asserted.
#   sentinel-pre-check7.log  real passes from before CHECK 7 logged its declines. The
#                            FIRST version of the starvation rule — "the pass logged no
#                            CHECK7 line, so it never reached the summon check" — fired on
#                            76 consecutive passes here, every one of them healthy: their
#                            silence was a fact about the sentinel's vocabulary at the
#                            time, not about its behaviour. The rule now reads `aeons=0`
#                            off the state line instead, and this fixture is what keeps it
#                            from regressing to an inference about an absent line.
#
# This suite needs nothing but python3 — auron-classify.py is already pure, and nothing
# here reaches a database or a file outside $TMP. The reconciler (raising, clearing and
# reopening real beads against it) is test-auron.sh.
# tier: T1
# covers: spira/auron-classify.py spira/testdata/sentinel-healthy.log spira/testdata/sentinel-pre-check7.log
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
DATA="$HERE/testdata"

# A fixed clock. Every threshold in the classifier is arithmetic on `now`, so a suite that
# used the real one would drift into and out of its own windows.
NOW=1800000000

keys_of() {   # keys_of <observations-json> -> the firing keys, comma separated, or `-`
    local got
    got="$(printf '%s' "$1" | python3 "$HERE/auron-classify.py" 2>/dev/null \
           | python3 -c '
import sys, json
ks = []
for line in sys.stdin:
    line = line.strip()
    if line:
        try: ks.append(json.loads(line)["key"])
        except Exception: pass
print(",".join(sorted(ks)))')"
    printf '%s' "${got:--}"
}

evidence_of() {  # evidence_of <observations-json> <key>
    printf '%s' "$1" | KEY="$2" python3 "$HERE/auron-classify.py" 2>/dev/null \
        | KEY="$2" python3 -c '
import sys, os, json
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    a = json.loads(line)
    if a["key"] == os.environ["KEY"]: sys.stdout.write(a["evidence"])'
}

check() {   # check <name> <expected-keys> <observations-json>
    local got; got="$(keys_of "$3")"
    [ "$got" = "$2" ] && ok "$1" || bad "$1" "expected [$2] got [$got]"
}

# obs <log-file-or-empty> [json-overrides] -> one observations object
# The base is a HEALTHY world: readable log, live timer, database up, no mirror
# configured, no strands. Every case below changes exactly one thing about it, which is
# what makes a firing key attributable to that one thing.
obs() {
    LOGF="${1:-}" OVER="${2:-{\}}" NOW="$NOW" python3 -c '
import json, os, sys
logf = os.environ.get("LOGF") or ""
text = open(logf, errors="replace").read() if logf else ""
o = {"now": int(os.environ["NOW"]), "auron_first": 0,
     "sentinel_log": text, "sentinel_log_readable": True, "sentinel_log_error": "",
     "sentinel_log_mtime": int(os.environ["NOW"]) - 30,
     "sentinel_log_path": "/run/sentinel.log", "sentinel_timer": "active",
     "db_reachable": True, "db_error": "", "db_path": "/db", "fallback_path": "/run/a.json",
     "mirror": {"configured": False}, "strands": {},
     "thresholds": {"pass_stale": 600, "starve_passes": 5,
                    "mirror_stale": 90000, "ghost_stale": 1800}}
o.update(json.loads(os.environ["OVER"]))
json.dump(o, sys.stdout)'
}

# mklog — a synthetic sentinel log, in the format lib.sh's log() actually writes.
#   mklog <passes> <ready> <aeons> <summoned:0|1> <complete:0|1> <last-pass-ends-at>
mklog() {
    N="$1" READY="$2" AEONS="$3" SUMM="$4" DONE="$5" END="$6" EXTRA="${7:-}" python3 -c '
import datetime, os
n, end = int(os.environ["N"]), int(os.environ["END"])
out = []
for i in range(n):
    t = end - (n - 1 - i) * 120
    ts = datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    out.append("%s spira: state: goal=sp-spira open=20 plan_ready=%s in_progress=0 aeons=%s fayths=[builder ]"
               % (ts, os.environ["READY"], os.environ["AEONS"]))
    if os.environ["EXTRA"]:
        out.append("%s spira: %s" % (ts, os.environ["EXTRA"]))
    if os.environ["SUMM"] == "1":
        out.append("%s spira: CHECK7 builder: %s ready, 1 free \u2014 summoning" % (ts, os.environ["READY"]))
    if os.environ["DONE"] == "1":
        out.append("%s spira: pass complete \u2014 0 action(s), 0 progress" % ts)
print("\n".join(out))'
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "auron-classify — the sentinel's pulse:"

mklog 10 5 1 1 1 $(( NOW - 60 )) > "$TMP/healthy.log"
check "healthy loop fires nothing" - "$(obs "$TMP/healthy.log")"

# Wedged: passes keep starting, none finishes. The log is still being written, which is
# what separates this from a dead timer — and the evidence has to say which it is.
mklog 10 5 0 0 0 $(( NOW - 60 )) > "$TMP/wedged.log"
check "no completed pass in the window" sentinel-stalled "$(obs "$TMP/wedged.log")"
ev="$(evidence_of "$(obs "$TMP/wedged.log")" sentinel-stalled)"
case "$ev" in *"wedged mid-flight"*) ok "wedged pass names its shape" ;;
    *) bad "wedged pass names its shape" "no 'wedged mid-flight' in the evidence" ;; esac

# Dead: nothing written for an hour and the timer is not active.
mklog 10 5 0 0 1 $(( NOW - 4000 )) > "$TMP/dead.log"
dead_obs="$(obs "$TMP/dead.log" '{"sentinel_timer":"inactive","sentinel_log_mtime":1799996000}')"
check "a dead timer is a stalled loop" sentinel-stalled "$dead_obs"
ev="$(evidence_of "$dead_obs" sentinel-stalled)"
case "$ev" in *"is not being run at all"*) ok "a dead timer names itself" ;;
    *) bad "a dead timer names itself" "the evidence does not name the timer" ;; esac

# THE FLOOR, AND EXACTLY WHAT IT IS FOR. A tail holding no completed pass AT ALL is
# ambiguous — it is either a loop that stopped long ago or a box where Auron came up
# first — so it is measured from whichever is later, the tail's own start or Auron's first
# run. A stall Auron can actually SEE the end of is not covered by this and must still
# fire, however new Auron is; the floor is for the case with no evidence, not for
# suppressing evidence.
mklog 10 5 0 0 0 $(( NOW - 60 )) > "$TMP/nodone.log"
check "no completion in the tail, and Auron only just started" - \
    "$(obs "$TMP/nodone.log" "{\"auron_first\":$(( NOW - 60 ))}")"
check "no completion in the tail, and Auron has been up all along" sentinel-stalled \
    "$(obs "$TMP/nodone.log" "{\"auron_first\":$(( NOW - 86400 ))}")"
check "a stall Auron can see the end of fires however new Auron is" sentinel-stalled \
    "$(obs "$TMP/dead.log" "{\"auron_first\":$(( NOW - 60 )),\"sentinel_log_mtime\":1799996000}")"

# AN UNREADABLE LOG IS THE ALERT. "I could not look" and "all clear" must never render as
# the same pixels (law-absence-needs-a-positive-control).
check "an unreadable log is itself the alert" sentinel-unobservable \
    "$(obs "" '{"sentinel_log_readable":false,"sentinel_log_error":"no such file"}')"

echo
echo "auron-classify — deliberate halt or drain suppresses sentinel-stalled:"

# POSITIVE CONTROL: the 'a dead timer is a stalled loop' case above proves this scenario
# fires without halt/drain flags. These are believed only because that one fired first.
check "halted loop does not fire sentinel-stalled" - \
    "$(obs "$TMP/dead.log" \
        "{\"world_halted\":true,\"world_halt_at\":$(( NOW - 900 )),\"sentinel_timer\":\"inactive\",\"sentinel_log_mtime\":$(( NOW - 3900 ))}")"
check "draining loop does not fire sentinel-stalled" - \
    "$(obs "$TMP/dead.log" \
        "{\"world_draining\":true,\"world_drain_at\":$(( NOW - 300 ))}")"

echo
echo "auron-classify — summon starvation:"

mklog 6 15 0 0 1 $(( NOW - 60 )) > "$TMP/starved.log"
check "ready work, zero aeons, nothing summoned, 5+ passes" summon-starved \
    "$(obs "$TMP/starved.log")"

mklog 4 15 0 0 1 $(( NOW - 60 )) > "$TMP/starved4.log"
check "four starved passes is not yet sustained" - "$(obs "$TMP/starved4.log")"

# The deliberate withhold. It is not a stall: it is the account's own five-hour window
# declining. A watchdog that alerts through a decision the harness made on purpose is pure
# noise.
mklog 6 15 0 0 1 $(( NOW - 60 )) "CHECK7 builder: the account is out of capacity for another 900s — not summoning" \
    > "$TMP/cap.log"
check "an exhausted account is not starvation" - "$(obs "$TMP/cap.log")"

# Aeons running IS the concurrency cap, which is throughput, not starvation.
mklog 6 15 2 0 1 $(( NOW - 60 )) > "$TMP/busy.log"
check "aeons at the cap is throughput, not starvation" - "$(obs "$TMP/busy.log")"

# Starvation over a window that has itself gone stale is not a second alert, it is a
# false one: nothing summoned in five passes says nothing when no sixth was attempted.
mklog 6 15 0 0 1 $(( NOW - 4000 )) > "$TMP/staleStarve.log"
check "starvation is not reported over a window that has gone stale" sentinel-stalled \
    "$(obs "$TMP/staleStarve.log" '{"sentinel_log_mtime":1799996000}')"

# The pass in flight has not reached CHECK 7 yet. Counting it would read the first twenty
# seconds of every ordinary pass as a refusal to summon.
{ mklog 5 15 1 1 1 $(( NOW - 180 )); mklog 1 15 0 0 0 $(( NOW - 20 )); } > "$TMP/inflight.log"
check "the pass in flight is not counted as a decline" - "$(obs "$TMP/inflight.log")"

echo
echo "auron-classify — replayed over real sentinel passes:"

if [ -r "$DATA/sentinel-healthy.log" ]; then
    # `now` is pinned just after the fixture's last line so the window is the real one.
    end="$(python3 -c '
import sys, re, datetime
last = 0
for l in open(sys.argv[1], errors="replace"):
    m = re.match(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)Z ", l)
    if m: last = int(datetime.datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=datetime.timezone.utc).timestamp())
print(last)' "$DATA/sentinel-healthy.log")"
    NOW=$(( end + 60 ))
    check "three hours of real healthy passes fire nothing" - "$(obs "$DATA/sentinel-healthy.log")"
    NOW=1800000000
else
    bad "real healthy passes" "$DATA/sentinel-healthy.log is missing — the noise floor is unmeasured"
fi

if [ -r "$DATA/sentinel-pre-check7.log" ]; then
    end="$(python3 -c '
import sys, re, datetime
last = 0
for l in open(sys.argv[1], errors="replace"):
    m = re.match(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)Z ", l)
    if m: last = int(datetime.datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=datetime.timezone.utc).timestamp())
print(last)' "$DATA/sentinel-pre-check7.log")"
    NOW=$(( end + 60 ))
    got="$(keys_of "$(obs "$DATA/sentinel-pre-check7.log")")"
    case ",$got," in
        *,summon-starved,*) bad "the 76-pass false positive stays quiet" \
            "summon-starved fired over the pre-CHECK7 window; the rule has regressed to inferring from an absent line" ;;
        *) ok "the 76-pass false positive stays quiet" ;;
    esac
    NOW=1800000000
else
    bad "pre-CHECK7 replay" "$DATA/sentinel-pre-check7.log is missing"
fi

echo
echo "auron-classify — the database, the mirror and the reaper:"

check "an unreachable database is an alert" db-unreachable \
    "$(obs "$TMP/healthy.log" '{"db_reachable":false,"db_error":"connection refused"}')"

check "no exporter configured means nothing to be stale" - \
    "$(obs "$TMP/healthy.log" '{"mirror":{"configured":false}}')"
check "an exporter that has written nothing is an alert" mirror-stale \
    "$(obs "$TMP/healthy.log" '{"mirror":{"configured":true,"exists":false,"path":"/m","exporter":"/e"}}')"
check "a mirror written an hour ago is fine" - \
    "$(obs "$TMP/healthy.log" '{"mirror":{"configured":true,"exists":true,"mtime":1799996400,"path":"/m","exporter":"/e"}}')"
check "a mirror older than a day is an alert" mirror-stale \
    "$(obs "$TMP/healthy.log" '{"mirror":{"configured":true,"exists":true,"mtime":1799800000,"path":"/m","exporter":"/e"}}')"

# The subject is the REAPER, not the ghost. strand.sh reclaims one within a pass, so a
# ghost still standing half an hour later means the reclaim is not working.
check "a fresh ghost lease is strand.sh's business, not Auron's" - \
    "$(obs "$TMP/healthy.log" '{"strands":{"ghost:sp-a":{"first":1799999000,"acted":0,"escalated":0}}}')"
check "a ghost lease nobody reclaimed is an alert" lease-unreclaimed \
    "$(obs "$TMP/healthy.log" '{"strands":{"ghost:sp-a":{"first":1799990000,"acted":1,"escalated":1}}}')"
check "other strand kinds are not Auron's business" - \
    "$(obs "$TMP/healthy.log" '{"strands":{"waiting:sp-e":{"first":1799000000}}}')"

# Two conditions at once are two alerts, not one merged report.
check "conditions do not mask each other" db-unreachable,sentinel-stalled \
    "$(obs "$TMP/wedged.log" '{"db_reachable":false,"db_error":"x"}')"

echo
echo "auron-classify — the log format it actually reads:"
# `ready=` was the field name before the plan's count was named apart from every fayth's.
# A tail that still holds those lines must parse, not be silently skipped as unmatched.
sed 's/plan_ready=/ready=/' "$TMP/starved.log" > "$TMP/oldfmt.log"
check "the older state-line spelling still parses" summon-starved "$(obs "$TMP/oldfmt.log")"
# Lines with no timestamp — a git merge notice, sending.sh's REAPED rows — are skipped.
{ echo "Auto-merging spira/sentinel.sh"; echo "REAPED sp-x  branch and worktree";
  cat "$TMP/healthy.log"; } > "$TMP/noise.log"
check "untimestamped lines are skipped, not misparsed" - "$(obs "$TMP/noise.log")"

echo
echo "auron-classify — unit restart loops:"

# obs_r <restart-alerts-json> [json-overrides] -> observations with restart_alerts
obs_r() {
    OVER="${2:-{\}}" NOW="$NOW" RA="$1" python3 -c '
import json, os, sys
o = {"now": int(os.environ["NOW"]), "auron_first": int(os.environ["NOW"]),
     "sentinel_log": "", "sentinel_log_readable": True, "sentinel_log_error": "",
     "sentinel_log_mtime": int(os.environ["NOW"]) - 30,
     "sentinel_log_path": "/run/sentinel.log", "sentinel_timer": "active",
     "db_reachable": True, "db_error": "", "db_path": "/db", "fallback_path": "/run/a.json",
     "mirror": {"configured": False}, "strands": {},
     "restart_alerts": json.loads(os.environ["RA"]),
     "thresholds": {"pass_stale": 600, "starve_passes": 5,
                    "mirror_stale": 90000, "ghost_stale": 1800,
                    "restart_threshold": 5, "restart_window": 3600}}
o.update(json.loads(os.environ["OVER"]))
json.dump(o, sys.stdout)' 2>/dev/null
}

# A unit whose restart count rose past the threshold fires.
_ra='[{"unit":"spira-cockpit-prod.service","current":255,"baseline":0,"delta":255,"window":3600,"journal":"last line"}]'
check "restart count above threshold fires" "restart-loop:spira-cockpit-prod.service" \
    "$(obs_r "$_ra")"
ev="$(evidence_of "$(obs_r "$_ra")" "restart-loop:spira-cockpit-prod.service")"
case "$ev" in *"255"*) ok "evidence carries the restart count" ;;
    *) bad "evidence carries the restart count" "count 255 not in evidence" ;; esac
case "$ev" in *"spira-cockpit-prod.service"*) ok "evidence names the unit" ;;
    *) bad "evidence names the unit" "unit name not in evidence" ;; esac
case "$ev" in *"incident intake misses it"*) ok "evidence explains why incident intake misses this" ;;
    *) bad "evidence explains why incident intake misses this" "explanation absent" ;; esac

# A count below the threshold does not fire.
_ra_low='[{"unit":"spira-cockpit-prod.service","current":3,"baseline":0,"delta":3,"window":3600,"journal":""}]'
check "restart count below threshold fires nothing" - "$(obs_r "$_ra_low")"

# An empty restart_alerts list fires nothing.
check "no restart alerts fires nothing" - "$(obs_r '[]')"

# Two units simultaneously: two distinct keys.
_ra_two='[{"unit":"spira-cockpit-prod.service","current":50,"baseline":0,"delta":50,"window":3600,"journal":""},
           {"unit":"spira-sentinel-prod.service","current":20,"baseline":0,"delta":20,"window":3600,"journal":""}]'
got_two="$(keys_of "$(obs_r "$_ra_two")")"
case ",$got_two," in
    *,restart-loop:spira-cockpit-prod.service,*) ok "first unit fires" ;;
    *) bad "first unit fires" "cockpit not in [$got_two]" ;;
esac
case ",$got_two," in
    *,restart-loop:spira-sentinel-prod.service,*) ok "second unit fires independently" ;;
    *) bad "second unit fires independently" "sentinel not in [$got_two]" ;;
esac

unset _ra _ra_low _ra_two got_two ev

tl_summary
