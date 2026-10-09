#!/usr/bin/env bash
#
# test-auron.sh — the watchdog's reconciler: raised, cleared and re-raised against a real bd.
#
#   ./test-auron.sh
#
# The classifier half (auron-classify.py, the pure function that turns observations into
# firing keys) is test-auron-classify.sh, which needs nothing but python and runs
# everywhere. This suite drives a real `bd` against a throwaway Dolt database, because a
# stub for bd has twice made correct callers look broken (law-prefer-the-real-dependency).
# With no server, this suite exits 77 — the automake skip convention, which gate-brain.sh
# names in the gate's output — but ONLY if everything that did run passed. A skip must
# never be able to swallow a failure.
#
# mklog (a synthetic sentinel log in lib.sh's log() format) is needed here too, for the
# wedge()/heal() fixtures below — it is duplicated from test-auron-classify.sh rather than
# shared, since testlib.sh suites are sourced standalone with no shared-helper convention.
#
# auron.sh is retired into the `auron` binary (sp-zpaq0, rewrite wave 5); this suite now
# drives that binary (`command auron --home "$SH"`) instead of `bash "$SH/auron.sh"`,
# exactly test-aeon-sweep.sh's own `command aeon --home ...` pattern for the same reason
# (the wrapper function below is itself named `auron`, shadowing the bare binary name).
# tier: T2
# covers: auron/src/* spira/conf.sh UC-ops-detection-remediation-19
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v auron >/dev/null 2>&1 \
    || { echo "test-auron: auron is not on PATH" >&2; exit 1; }

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
    out.append("%s spira: state: open=20 plan_ready=%s in_progress=0 aeons=%s fayths=[builder ]"
               % (ts, os.environ["READY"], os.environ["AEONS"]))
    if os.environ["EXTRA"]:
        out.append("%s spira: %s" % (ts, os.environ["EXTRA"]))
    if os.environ["SUMM"] == "1":
        out.append("%s spira: CHECK7 builder: %s ready, 1 free — summoning" % (ts, os.environ["READY"]))
    if os.environ["DONE"] == "1":
        out.append("%s spira: pass complete — 0 action(s), 0 progress" % ts)
print("\n".join(out))'
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-auron.sh"

. "$HERE/testdb.sh"
if ! testdb_available; then
    printf 'SKIP test-auron: no bd engine available — the reconcile cases need a real bd.\n' \
        >&2
    printf '  embedded: install bd-embedded  server: set SPIRA_TESTDB_DATA in spira.conf\n' \
        >&2
    skip "no bd engine available"
fi
testdb_up auron || { echo "test-auron: could not build a fixture database"; exit 1; }

SH="$TMP/spira"; RUN="$TMP/run"; mkdir -p "$SH" "$RUN"
cp "$HERE/auron-classify.py" "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
# ITS OWN SPIRA_HOME AND ITS OWN repo-map. Without one, SPIRA_HOME falls back to the
# INSTALLED harness directory and this suite would read the operator's real repositories.
printf 'brain | %s | push | origin/main | |\n' "$TMP/repo" > "$SH/repo-map"

# A close goes through spira-lc (sp-3fue0j); this fixture has no lifecycle store, so it closes the
# store, through whichever bd and database the case declared last (read at each call).
lc_close_stub "$TMP/lc"
auron() {   # auron [--report] — one run against the fixture, with a chosen database
    # SPIRA_DB is registered and auron resolves it via cfg(), not env (confirmed in
    # auron/src/main.rs) — the AURON_DB override must go through tl_config too, or the
    # fallback/saturation cases silently keep using the real, reachable database.
    tl_config SPIRA_RUN="$RUN" SPIRA_EXPORTER="" SPIRA_DB="${AURON_DB:-$SPIRA_DB}"
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$TMP/repo" SPIRA_SYSTEMCTL=true \
    SPIRA_AURON_SENTINEL_LOG="$RUN/sentinel.log" \
        command auron --home "$SH" "$@" 2>&1
}
alert_status() {   # alert_status <key> -> "<id> <status>", or "-" if there is no bead
    timeout 5 bd -C "$SPIRA_DB" list --all --limit 0 --label alert --json 2>/dev/null \
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
    timeout 5 bd -C "$SPIRA_DB" list --all --limit 0 --label alert --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))'
}
n_probe_beads() {   # open + closed auron:probe beads
    timeout 5 bd -C "$SPIRA_DB" list --all --limit 0 --label auron:probe --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))'
}
create_probe() {    # create_probe -> id on stdout
    timeout 5 bd -C "$SPIRA_DB" create --title "Auron write probe" --type event -p 0 \
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
kind="$(timeout 5 bd -C "$SPIRA_DB" show "$ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print("%s|%s" % (d.get("issue_type"), ",".join(sorted(d.get("labels") or []))))' 2>/dev/null)"
is "an event labelled alert/overseer/flaps, and NOT needs-ryan" \
   "event|alert,alert:sentinel-stalled,flaps:1,overseer" "$kind"

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
lab="$(timeout 5 bd -C "$SPIRA_DB" show "$ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(",".join(sorted(l for l in (d.get("labels") or []) if l.startswith("flaps:"))))' 2>/dev/null)"
is "the pane's flaps: label is the count, and there is only one" "flaps:2" "$lab"

# `acked` says the operator has SEEN this occurrence. Carrying it into the next one hides
# exactly what the flap count exists to surface, so a returning condition clears it — while
# `silent-until:` is the pane's own affordance and Auron must never touch it, or a silence
# the operator asked for would evaporate on the next pass.
timeout 5 bd -C "$SPIRA_DB" update "$ID" --add-label acked >/dev/null 2>&1
timeout 5 bd -C "$SPIRA_DB" update "$ID" --add-label "silent-until:2099-01-01T00:00:00Z" >/dev/null 2>&1
heal; auron >/dev/null; auron >/dev/null       # clears
wedge; auron >/dev/null; auron >/dev/null      # and returns
lab="$(timeout 5 bd -C "$SPIRA_DB" show "$ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
ls = d.get("labels") or []
print("%s %s" % ("acked" in ls, any(l.startswith("silent-until:") for l in ls)))' 2>/dev/null)"
is "a returning condition clears acked and leaves silent-until alone (acked cleared, silence kept)" \
   "False True" "$lab"

echo
echo "auron.sh — the operator's own close, and the channel of last resort:"

# Closing it by hand is an acknowledgement. Re-raising it would be a machine arguing with
# the person it is reporting to. The operator's close is fixture state, declared as data
# (sp-voip5): what is under test is Auron's answer to a closed row, not bd's close verb.
testdb_restate "$ID" closed
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
tl_config SPIRA_BD="$TESTDB_BD"
auron >/dev/null
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
tl_config SPIRA_BD="$WRITE_FAIL_BD"
auron >/dev/null
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
tl_config SPIRA_BD="$TESTDB_BD"
auron >/dev/null
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
tl_config SPIRA_BD="$REDERIVE_FAIL_BD"
auron >/dev/null
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
    tl_config SPIRA_BD="$TESTDB_BD"
    auron >/dev/null
    # Exactly 1 probe bead should be open; all others (pre-existing + 2 of the 3 new ones)
    # should be closed rather than deleted.
    n_open="$(timeout 5 bd -C "$SPIRA_DB" list --limit 0 --label auron:probe --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))')"
    is "extras: only one probe bead is open after adoption" 1 "$n_open"
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
tl_config SPIRA_BD="$TIMEOUT_BD"
AURON_DB=/nonexistent-spira-db auron >/dev/null
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

# Reset SPIRA_BD back to the real store: it stays declared in the override file (there is
# no implicit per-call env scoping any more) until something changes it again, so every
# bare `auron`/`auron_restart` call below would otherwise keep hitting the 124-exit stub.
tl_config SPIRA_BD="$TESTDB_BD"

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
    # SPIRA_DB via tl_config too: auron resolves it via cfg(), not the plain env prefix
    # below (same gap as auron()'s own call above it, line 78).
    tl_config SPIRA_RUN="$RUN" SPIRA_EXPORTER="" \
        SPIRA_AURON_RESTARTS=3 SPIRA_AURON_RESTART_WINDOW=3600 \
        SPIRA_DB="${AURON_DB:-$SPIRA_DB}"
    SPIRA_HOME="$SH" SPIRA_DB="${AURON_DB:-$SPIRA_DB}" \
    SPIRA_REPO="$TMP/repo" \
    SPIRA_SYSTEMCTL="$RESTART_SC" \
    SPIRA_AURON_SENTINEL_LOG="$RUN/sentinel.log" \
        command auron --home "$SH" "$@" 2>&1
}
restart_alert_status() {
    timeout 5 bd -C "$SPIRA_DB" list --all --limit 0 --label alert --json 2>/dev/null \
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
_rst_n="$(timeout 5 bd -C "$SPIRA_DB" list --all --limit 0 --label "alert:restart-loop:spira-cockpit-prod.service" --json 2>/dev/null \
    | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))')"
is "restart: a second pass on the same standing loop does not raise a duplicate" 1 "$_rst_n"

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
