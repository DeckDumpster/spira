#!/usr/bin/env bash
#
# test-auron.sh — the watchdog, seen firing and seen staying quiet.
#
#   ./test-auron.sh
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
# THE SKIP RULE. The classifier half needs nothing but python and runs everywhere. The
# reconcile half drives a real `bd` against a throwaway Dolt database, because a stub for
# bd has twice made correct callers look broken (law-prefer-the-real-dependency). With no
# server, this suite exits 77 — the automake skip convention, which gate-brain.sh names in
# the gate's output — but ONLY if everything that did run passed. A skip must never be able
# to swallow a failure.
# tier: T2
# covers: spira/auron.sh spira/auron-classify.py spira/conf.sh spira/testdata/sentinel-healthy.log spira/testdata/sentinel-pre-check7.log
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
DATA="$HERE/testdata"
pass=0; fail=0


# A fixed clock. Every threshold in the classifier is arithmetic on `now`, so a suite that
# used the real one would drift into and out of its own windows.
NOW=1800000000

# ======================================================================================
# PART 1 — the classifier, as the pure function it is.
# ======================================================================================
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

# ======================================================================================
# PART 2 — the reconciler, against a real bd.
# ======================================================================================
echo
. "$HERE/testdb.sh"
if ! testdb_available; then
    printf '\n  %d passed, %d failed (classifier)\n' "$pass" "$fail"
    printf 'SKIP test-auron: no bd engine available — the reconcile cases need a real bd.\n' \
        >&2
    printf '  embedded: install bd-embedded  server: set SPIRA_TESTDB_DATA in spira.conf\n' \
        >&2
    # A SKIP MUST NOT SWALLOW A FAILURE. Everything above ran without a server; if any of
    # it failed, this suite failed, and 77 would hide that behind the gate's "SKIPPED".
    [ "$fail" -eq 0 ] || exit 1
    exit 77
fi
testdb_up auron || { echo "test-auron: could not build a fixture database"; exit 1; }

SH="$TMP/spira"; RUN="$TMP/run"; mkdir -p "$SH" "$RUN"
cp "$HERE/auron.sh" "$HERE/auron-classify.py" "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
# ITS OWN SPIRA_HOME AND ITS OWN repo-map. Without one, SPIRA_HOME falls back to the
# INSTALLED harness directory and this suite would read the operator's real repositories.
printf 'brain | %s | push | origin/main | |\n' "$TMP/repo" > "$SH/repo-map"

auron() {   # auron [--report] — one run against the fixture, with a chosen database
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="${AURON_DB:-$SPIRA_DB}" \
    SPIRA_REPO="$TMP/repo" SPIRA_EXPORTER="" SPIRA_SYSTEMCTL=true \
    SPIRA_AURON_SENTINEL_LOG="$RUN/sentinel.log" \
        "$SH/auron.sh" "$@" 2>&1
}
alert_status() {   # alert_status <key> -> "<id> <status>", or "-" if there is no bead
    bd -C "$SPIRA_DB" list --all --limit 0 --label alert --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | KEY="$1" python3 -c '
import sys, os, json
try: d = json.load(sys.stdin)
except Exception: d = []
for i in (d if isinstance(d, list) else [d]):
    if "alert:" + os.environ["KEY"] in (i.get("labels") or []):
        print("%s %s" % (i["id"], i.get("status"))); break
else:
    print("-")'
}
n_alert_beads() {
    bd -C "$SPIRA_DB" list --all --limit 0 --label alert --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))'
}
n_probe_beads() {   # open + closed auron:probe beads
    bd -C "$SPIRA_DB" list --all --limit 0 --label auron:probe --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))'
}
create_probe() {    # create_probe -> id on stdout
    bd -C "$SPIRA_DB" create --title "Auron write probe" --type event -p 0 \
        --labels "auron:probe,overseer" \
        --body "write-path probe — updated on every Auron pass" --json 2>/dev/null \
        | python3 -c '
import sys, json
t = sys.stdin.read(); i = t.find("{")
if i >= 0:
    try: print(json.loads(t[i:])["id"])
    except Exception: pass' 2>/dev/null
}
# A REALISTIC WEDGE: passes completed normally until an hour ago, then kept starting and
# stopped finishing. A log with no completed pass ANYWHERE cannot distinguish a wedged
# loop from a box where Auron came up first, and the floor deliberately reads it as the
# second — so a fixture without completions would test the floor, not the wedge.
wedge()  { { mklog 6 5 1 1 1 "$(( $(date +%s) - 3600 ))"
             mklog 10 5 0 0 0 "$(date +%s)"; } > "$RUN/sentinel.log"; }
heal()   { mklog 10 5 1 1 1 "$(date +%s)" > "$RUN/sentinel.log"; }

echo "auron.sh — one bead per cause, raised, cleared and re-raised:"

wedge
auron >/dev/null
[ "$(alert_status sentinel-stalled)" = "-" ] \
    && ok "one sighting does not fire — a condition must be confirmed" \
    || bad "confirm" "an alert was raised on the first sighting"
[ -r "$RUN/auron.status" ] && ok "the heartbeat is written even when nothing fires" \
    || bad "heartbeat" "no $RUN/auron.status after a quiet run"

auron >/dev/null
st="$(alert_status sentinel-stalled)"
case "$st" in *" open") ok "a confirmed condition raises an open alert bead" ;;
    *) bad "raise" "expected an open bead, got [$st]" ;; esac
ID="${st%% *}"
# THE WRITER'S HALF OF A CONTRACT WITH THE ATTENTION PANE. The ALERTS tab selects on
# `alert` AND `overseer` and reads the count out of `flaps:<n>`; those three strings are
# the whole interface between this program and a Rust binary in another directory, and
# nothing about either half's code would announce a drift. `needs-ryan` must be absent, or
# the bead lands in DECISIONS — the one list whose value is that nothing leaves it unless
# the operator moved it.
kind="$(bd -C "$SPIRA_DB" show "$ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print("%s|%s" % (d.get("issue_type"), ",".join(sorted(d.get("labels") or []))))' 2>/dev/null)"
[ "$kind" = "event|alert,alert:sentinel-stalled,flaps:1,overseer" ] \
    && ok "an event labelled alert/overseer/flaps, and NOT needs-ryan" \
    || bad "shape" "expected [event|alert,alert:sentinel-stalled,flaps:1,overseer] got [$kind]"

auron >/dev/null
[ "$(n_alert_beads)" = 1 ] && ok "a condition that keeps holding does not open a second bead" \
    || bad "duplicates" "$(n_alert_beads) alert beads after three runs of one condition"

heal; auron >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID open" ] \
    && ok "one clear sighting does not retract it" \
    || bad "clear hysteresis" "the alert was retracted on the first quiet run"
auron >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID closed" ] \
    && ok "Auron closes its own alert when the condition passes" \
    || bad "clear" "expected [$ID closed] got [$(alert_status sentinel-stalled)]"

wedge; auron >/dev/null; auron >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID open" ] \
    && ok "the condition returning reopens the SAME bead" \
    || bad "reopen" "expected [$ID open] got [$(alert_status sentinel-stalled)]"
[ "$(n_alert_beads)" = 1 ] && ok "a flap does not accumulate beads" \
    || bad "flap" "$(n_alert_beads) beads after one flap — a pane becomes wallpaper this way"
flaps="$(awk -F'\t' '$1=="sentinel-stalled"{print $7}' "$RUN/auron.state")"
[ "$flaps" = 2 ] && ok "the flap count reached 2" || bad "flaps" "expected 2 got [$flaps]"
# ONE CURRENT VALUE, NOT AN ACCUMULATION. `--add-label` alone would leave `flaps:1` beside
# `flaps:2`, and the pane reads the first it finds — so the count would freeze at 1 while
# the state file went on counting, and the number the operator sees is the one that matters.
lab="$(bd -C "$SPIRA_DB" show "$ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(",".join(sorted(l for l in (d.get("labels") or []) if l.startswith("flaps:"))))' 2>/dev/null)"
[ "$lab" = "flaps:2" ] && ok "the pane's flaps: label is the count, and there is only one" \
    || bad "flaps label" "expected [flaps:2] got [$lab]"

# `acked` says the operator has SEEN this occurrence. Carrying it into the next one hides
# exactly what the flap count exists to surface, so a returning condition clears it — while
# `silent-until:` is the pane's own affordance and Auron must never touch it, or a silence
# the operator asked for would evaporate on the next pass.
bd -C "$SPIRA_DB" update "$ID" --add-label acked >/dev/null 2>&1
bd -C "$SPIRA_DB" update "$ID" --add-label "silent-until:2099-01-01T00:00:00Z" >/dev/null 2>&1
heal; auron >/dev/null; auron >/dev/null       # clears
wedge; auron >/dev/null; auron >/dev/null      # and returns
lab="$(bd -C "$SPIRA_DB" show "$ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
ls = d.get("labels") or []
print("%s %s" % ("acked" in ls, any(l.startswith("silent-until:") for l in ls)))' 2>/dev/null)"
[ "$lab" = "False True" ] \
    && ok "a returning condition clears acked and leaves silent-until alone" \
    || bad "acked/silent" "expected [False True] (acked cleared, silence kept) got [$lab]"

echo
echo "auron.sh — the operator's own close, and the channel of last resort:"

# Closing it by hand is an acknowledgement. Re-raising it would be a machine arguing with
# the person it is reporting to.
bd -C "$SPIRA_DB" close "$ID" --reason "acknowledged" >/dev/null 2>&1
sed -i 's/\t[0-9]*$/\t0/' "$RUN/auron.state"      # force the hourly refresh window open
auron >/dev/null; auron >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID closed" ] \
    && ok "an alert closed by hand is not reopened while it still fires" \
    || bad "acknowledge" "Auron reopened a bead the operator had closed"

AURON_DB=/nonexistent-spira-db auron >/dev/null
[ -r "$RUN/auron.alerts.json" ] && ok "an unreachable database falls back to the file channel" \
    || bad "fallback" "no fallback file was written with the database down"
fb="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print("%s %s" % (d["db_reachable"], ",".join(sorted(a["key"] for a in d["alerts"]))))' \
    "$RUN/auron.alerts.json" 2>/dev/null)"
case "$fb" in "False "*db-unreachable*) ok "the fallback carries the alerts and says the database is down" ;;
    *) bad "fallback content" "got [$fb]" ;; esac
grep -q 'SP_AURON_DB_READ=down' "$RUN/auron.status" \
    && ok "the heartbeat is written even when the database is down" \
    || bad "heartbeat under failure" "SP_AURON_DB_READ is not down in $RUN/auron.status"

auron >/dev/null
[ -e "$RUN/auron.alerts.json" ] \
    && bad "fallback removal" "the fallback file survived the database coming back — two sources of truth" \
    || ok "the fallback file is removed once beads answers again"

echo
echo "auron.sh — write probe detects a write-only database failure:"

# Build a write-fail shim. The seam SPIRA_BD exists for exactly this: a bd that fails
# in a way the real embedded database cannot be asked to reproduce on demand (the write-
# only failure that happens when a schema cursor is rolled back). The shim delegates
# reads to the real binary; writes return error. This isolates the write path without
# modelling bd's surface — reads still go through the real engine.
WRITE_FAIL_BD="$TMP/write-fail-bd"
{
    printf '#!/usr/bin/env bash\n'
    printf '# Passes reads through to the real embedded binary; fails writes.\n'
    printf '# Simulates a schema-cursor write failure (reads ok, writes refused).\n'
    printf 'case "${3:-}" in\n'
    printf '    list|show|migrate) exec %q "$@" ;;\n' "$TESTDB_BD"
    printf '    *) printf "write-fail-bd: write refused (simulating schema-skew write failure)\\n" >&2; exit 1 ;;\n'
    printf 'esac\n'
} > "$WRITE_FAIL_BD"
chmod +x "$WRITE_FAIL_BD"

# A healthy loop — no conditions firing. With a working database, the write probe
# creates its bead and publishes SP_AURON_DB_WRITE=ok.
heal; rm -f "$RUN/auron.state"   # clear state so probe starts fresh
SPIRA_BD="$TESTDB_BD" auron >/dev/null
grep -q 'SP_AURON_DB_WRITE=ok' "$RUN/auron.status" \
    && ok "write probe: healthy database shows write ok" \
    || bad "write probe baseline" "SP_AURON_DB_WRITE is not ok before the fault"
grep -q 'SP_AURON_DB_READ=ok' "$RUN/auron.status" \
    && ok "write probe: read path also ok at baseline" \
    || bad "write probe baseline read" "SP_AURON_DB_READ is not ok before the fault"
[ ! -e "$RUN/auron.alerts.json" ] \
    && ok "write probe: no fallback file when writes are healthy" \
    || bad "write probe baseline fallback" "fallback file exists when it should not"

# Now switch to the write-fail shim. Reads succeed, writes fail.
# The write probe detects the failure and the fallback file appears.
rm -f "$RUN/auron.state"   # clear state so probe tries to create (which will fail)
SPIRA_BD="$WRITE_FAIL_BD" auron >/dev/null
grep -q 'SP_AURON_DB_READ=ok' "$RUN/auron.status" \
    && ok "write probe: read path still shows ok during write-only failure" \
    || bad "write probe read during fault" "SP_AURON_DB_READ is not ok with writes broken"
grep -q 'SP_AURON_DB_WRITE=down' "$RUN/auron.status" \
    && ok "write probe: SP_AURON_DB_WRITE=down when writes fail" \
    || bad "write probe fault" "SP_AURON_DB_WRITE is not down with writes broken"
[ -r "$RUN/auron.alerts.json" ] \
    && ok "write probe: fallback file written when write path fails" \
    || bad "write probe fallback" "no fallback file when write probe failed"
# Reads are fine so db_reachable=1; the fallback reflects write failure, not read failure.
fb_write="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(d.get("db_reachable"), d.get("db_write_ok"))' "$RUN/auron.alerts.json" 2>/dev/null)"
[ "$fb_write" = "True False" ] \
    && ok "write probe: fallback carries db_reachable=True and db_write_ok=False" \
    || bad "write probe fallback content" "got [$fb_write]"

# Restore the healthy database. The fallback file is removed and writes are ok again.
SPIRA_BD="$TESTDB_BD" auron >/dev/null
grep -q 'SP_AURON_DB_WRITE=ok' "$RUN/auron.status" \
    && ok "write probe: write ok restored after the fault clears" \
    || bad "write probe restore" "SP_AURON_DB_WRITE is not ok after restoring the database"
[ ! -e "$RUN/auron.alerts.json" ] \
    && ok "write probe: fallback file removed once writes succeed again" \
    || bad "write probe fallback removal" "fallback file survived the database recovering"

echo
echo "auron.sh — write probe: failed re-derive does not create a bead (Fix 1):"

# A shim that passes the alert-list read through (so db_reachable=1) but fails the
# probe re-derive. Without Fix 1 the code created a new P0 bead every time the re-derive
# failed, because a failed call and an empty result were indistinguishable.
REDERIVE_FAIL_BD="$TMP/rederive-fail-bd"
{
    printf '#!/usr/bin/env bash\n'
    # The alert probe uses --label alert; the re-derive uses --label auron:probe.
    # Pass alert reads to the real binary so db_reachable stays 1; fail probe reads.
    printf 'for a in "$@"; do case "$a" in\n'
    printf '    "auron:probe") printf "rederive-fail-bd: probe list refused\\n" >&2; exit 1 ;;\n'
    printf 'esac; done\n'
    printf 'exec %q "$@"\n' "$TESTDB_BD"
} > "$REDERIVE_FAIL_BD"
chmod +x "$REDERIVE_FAIL_BD"

heal; rm -f "$RUN/auron.state"   # empty state: PROBE_ID empty, re-derive will run
_n_probes_before="$(n_probe_beads)"
SPIRA_BD="$REDERIVE_FAIL_BD" auron >/dev/null
_n_probes_after="$(n_probe_beads)"
[ "$_n_probes_after" = "$_n_probes_before" ] \
    && ok "re-derive fail: no probe bead created when re-derive fails" \
    || bad "re-derive fail" "probe bead was leaked when re-derive failed (before=$_n_probes_before after=$_n_probes_after)"
unset _n_probes_before _n_probes_after
grep -q 'SP_AURON_DB_WRITE=down' "$RUN/auron.status" \
    && ok "re-derive fail: write reported as down when re-derive fails" \
    || bad "re-derive fail write status" "SP_AURON_DB_WRITE should be down when re-derive failed"

echo
echo "auron.sh — write probe: extras closed on re-derive (Fix 3):"

# Pre-populate three probe beads (simulating the debris from past contention episodes).
# After one run with empty state, all but the first should be closed.
heal; rm -f "$RUN/auron.state"
_extras_before="$(n_probe_beads)"   # track pre-existing probes; test counts relative to this
_p1="$(create_probe)"; _p2="$(create_probe)"; _p3="$(create_probe)"
if [ -z "$_p1" ] || [ -z "$_p2" ] || [ -z "$_p3" ]; then
    bad "extras setup" "could not create three probe beads for extras test"
else
    SPIRA_BD="$TESTDB_BD" auron >/dev/null
    # Exactly 1 probe bead should be open; all others (pre-existing + 2 of the 3 new ones)
    # should be closed rather than deleted.
    n_open="$(bd -C "$SPIRA_DB" list --limit 0 --label auron:probe --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))')"
    [ "$n_open" = 1 ] \
        && ok "extras: only one probe bead is open after adoption" \
        || bad "extras" "expected 1 open probe bead, got [$n_open]"
    _expected_total=$(( _extras_before + 3 ))
    [ "$(n_probe_beads)" = "$_expected_total" ] \
        && ok "extras: all beads still exist (closed, not deleted)" \
        || bad "extras count" "expected $_expected_total total, got [$(n_probe_beads)]"
    grep -q 'SP_AURON_DB_WRITE=ok' "$RUN/auron.status" \
        && ok "extras: probe update succeeds after adoption" \
        || bad "extras write" "SP_AURON_DB_WRITE not ok after adoption"
fi
unset _p1 _p2 _p3 n_open _extras_before _expected_total

echo
echo "auron.sh — write probe: lock-timeout reported as saturated, not down (Fix 5):"

# A shim that exits 124 (the timeout exit code) for all calls. This simulates the lock
# contention that drove half of all passes to report DB_READ=down on a healthy database.
TIMEOUT_BD="$TMP/timeout-bd"
{
    printf '#!/usr/bin/env bash\n'
    printf 'exit 124\n'
} > "$TIMEOUT_BD"
chmod +x "$TIMEOUT_BD"

heal; rm -f "$RUN/auron.state"
AURON_DB=/nonexistent-spira-db SPIRA_BD="$TIMEOUT_BD" auron >/dev/null
grep -q 'SP_AURON_DB_READ=saturated' "$RUN/auron.status" \
    && ok "saturation: timeout on read probe reports DB_READ=saturated" \
    || bad "saturation read" "expected SP_AURON_DB_READ=saturated, got $(grep SP_AURON_DB_READ "$RUN/auron.status" 2>/dev/null || echo none)"
grep -q 'SP_AURON_DB_SATURATED=1' "$RUN/auron.status" \
    && ok "saturation: SP_AURON_DB_SATURATED=1 when timed out" \
    || bad "saturation flag" "SP_AURON_DB_SATURATED not 1"
# A timeout is still unreachable as far as the rest of the pass is concerned, so the
# fallback file should still be written (same as the db-down case).
[ -r "$RUN/auron.alerts.json" ] \
    && ok "saturation: fallback file written on timeout" \
    || bad "saturation fallback" "no fallback file on timeout"

echo
echo "auron.sh — restart loop detection via stub systemctl:"

# Build a stub systemctl that serves list-units and show responses so auron.sh sees
# a unit with a rising NRestarts count. RESTARTS_THRESHOLD is set to 3 so a count
# of 5 (above) and 2 (below) cleanly straddle the threshold.
RESTART_SC="$TMP/restart-sc"
{
    printf '#!/usr/bin/env bash\n'
    # Return a fixed count stored in a file so the test can change it between runs.
    printf 'COUNT_FILE=%q\n' "$TMP/restart-count"
    printf 'case "${*}" in\n'
    # list-units: return one spira service
    printf '  *"list-units"*)\n'
    printf '    printf "spira-cockpit-prod.service loaded active running\\n"\n'
    printf '    exit 0 ;;\n'
    # show --property=Id,NRestarts: return Id and current count
    printf '  *"show"*)\n'
    printf '    cnt=$(cat "$COUNT_FILE" 2>/dev/null || echo 0)\n'
    printf '    printf "Id=spira-cockpit-prod.service\\nNRestarts=%%s\\n\\n" "$cnt"\n'
    printf '    exit 0 ;;\n'
    # is-active (for sentinel timer): not active
    printf '  *) exit 0 ;;\n'
    printf 'esac\n'
} > "$RESTART_SC"
chmod +x "$RESTART_SC"

auron_restart() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="${AURON_DB:-$SPIRA_DB}" \
    SPIRA_REPO="$TMP/repo" SPIRA_EXPORTER="" \
    SPIRA_SYSTEMCTL="$RESTART_SC" \
    SPIRA_AURON_SENTINEL_LOG="$RUN/sentinel.log" \
    SPIRA_AURON_RESTARTS=3 SPIRA_AURON_RESTART_WINDOW=3600 \
        "$SH/auron.sh" "$@" 2>&1
}
restart_alert_status() {
    bd -C "$SPIRA_DB" list --all --limit 0 --label alert --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, os, json
key = "restart-loop:spira-cockpit-prod.service"
try: d = json.load(sys.stdin)
except Exception: d = []
for i in (d if isinstance(d, list) else [d]):
    if "alert:" + key in (i.get("labels") or []):
        print("%s %s" % (i["id"], i.get("status"))); break
else:
    print("-")'
}

heal  # healthy sentinel log so sentinel-stalled does not interfere
rm -f "$RUN/auron.state"

# Count below threshold: no alert.
printf '2\n' > "$TMP/restart-count"
auron_restart >/dev/null
[ "$(restart_alert_status)" = "-" ] \
    && ok "restart: count below threshold fires nothing" \
    || bad "restart below threshold" "alert raised when count was below threshold"

# Count above threshold but only one sighting: CONFIRM requires two.
printf '10\n' > "$TMP/restart-count"
auron_restart >/dev/null
[ "$(restart_alert_status)" = "-" ] \
    && ok "restart: one sighting does not fire — a condition must be confirmed" \
    || bad "restart confirm" "alert raised on first sighting above threshold"

# Second sighting: alert raised.
auron_restart >/dev/null
_rst="$(restart_alert_status)"
case "$_rst" in *" open") ok "restart: confirmed condition raises an open alert bead" ;;
    *) bad "restart raise" "expected an open bead, got [$_rst]" ;; esac
_rst_id="${_rst%% *}"

# Third pass on the same standing loop: no duplicate bead.
auron_restart >/dev/null
_rst_n="$(bd -C "$SPIRA_DB" list --all --limit 0 --label "alert:restart-loop:spira-cockpit-prod.service" --json 2>/dev/null \
    | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))')"
[ "$_rst_n" = 1 ] \
    && ok "restart: a second pass on the same standing loop does not raise a duplicate" \
    || bad "restart duplicate" "expected 1 alert bead, got [$_rst_n]"

# Count goes DOWN (daemon-reload): no new alert; baseline resets.
printf '0\n' > "$TMP/restart-count"
auron_restart >/dev/null; auron_restart >/dev/null
[ "$(restart_alert_status)" = "$_rst_id closed" ] \
    && ok "restart: alert cleared after restart count went down" \
    || bad "restart reset" "expected [$_rst_id closed] got [$(restart_alert_status)]"

# Count rises again above threshold: alert reopened.
printf '8\n' > "$TMP/restart-count"
auron_restart >/dev/null; auron_restart >/dev/null
[ "$(restart_alert_status)" = "$_rst_id open" ] \
    && ok "restart: count rising again reopens the same bead" \
    || bad "restart reopen" "expected [$_rst_id open] got [$(restart_alert_status)]"

unset _rst _rst_id _rst_n

echo
echo "auron.sh — deliberately halted loop does not raise sentinel-stalled:"

heal; rm -f "$RUN/auron.state"
printf '2026-01-01T00:00:00Z\nwhy: test halt\n' > "$RUN/world.halted"
wedge
auron >/dev/null; auron >/dev/null
# An earlier test already created a closed sentinel-stalled bead. The check is that no
# NEW OPEN bead was raised, not that no bead exists at all.
_st_halt="$(alert_status sentinel-stalled)"
case "$_st_halt" in
    "-"|*" closed") ok "a halted loop does not raise sentinel-stalled" ;;
    *) bad "halt suppress" "sentinel-stalled raised while loop is deliberately halted" ;;
esac
unset _st_halt
rm -f "$RUN/world.halted"
# Without the halt stamp, the same wedged log fires after CONFIRM=2 passes.
auron >/dev/null; auron >/dev/null
st_h="$(alert_status sentinel-stalled)"
case "$st_h" in *" open") ok "lifting the halt stamp restores normal alerting" ;;
    *) bad "halt lifted" "expected sentinel-stalled open after halt stamp removed, got [$st_h]" ;; esac
unset st_h

testdb_drop >/dev/null 2>&1
tl_summary
