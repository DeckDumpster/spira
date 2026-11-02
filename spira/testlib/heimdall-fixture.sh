# Shared fixture of the test-heimdall*.sh suites: the synthetic sentinel log, the harness copy the
# binary runs against, and the bd-reading helpers. Sourced after testdb_up, with $HERE and $TMP set.

# mklog — a synthetic sentinel log, in the format lib.sh's log() actually writes.
#   mklog <passes> <ready> <dwarves> <called:0|1> <complete:0|1> <last-pass-ends-at>
mklog() {
    N="$1" READY="$2" DWARVES="$3" SUMM="$4" DONE="$5" END="$6" EXTRA="${7:-}" python3 -c '
import datetime, os
n, end = int(os.environ["N"]), int(os.environ["END"])
out = []
for i in range(n):
    t = end - (n - 1 - i) * 120
    ts = datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    out.append("%s spira: state: open=20 plan_ready=%s in_progress=0 dwarves=%s runes=[builder ]"
               % (ts, os.environ["READY"], os.environ["DWARVES"]))
    if os.environ["EXTRA"]:
        out.append("%s spira: %s" % (ts, os.environ["EXTRA"]))
    if os.environ["SUMM"] == "1":
        out.append("%s spira: CHECK7 builder: %s ready, 1 free — calling" % (ts, os.environ["READY"]))
    if os.environ["DONE"] == "1":
        out.append("%s spira: pass complete — 0 action(s), 0 progress" % ts)
print("\n".join(out))'
}

SH="$TMP/spira"; RUN="$TMP/run"; mkdir -p "$SH" "$RUN"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SH/"
cp -r "$HERE/conf.d" "$HERE/conf.toml.d" "$HERE/conf-gen.sh" "$SH/"

# A close goes through spira-lc (sp-3fue0j); this fixture has no lifecycle store, so it closes the
# store, through whichever bd and database the case declared last (read at each call).
lc_close_stub "$TMP/lc"
heimdall() {   # heimdall [--report] — one run against the fixture, with a chosen database
    # SPIRA_DB is registered and heimdall resolves it via cfg(), not env (confirmed in
    # heimdall/src/main.rs) — the HEIMDALL_DB override must go through tl_config too, or the
    # fallback/saturation cases silently keep using the real, reachable database.
    tl_config SPIRA_RUN="$RUN" SPIRA_EXPORTER="" SPIRA_WIKI="" SPIRA_DB="${HEIMDALL_DB:-$SPIRA_DB}"
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$TMP/repo" SPIRA_SYSTEMCTL=true \
    SPIRA_HEIMDALL_SENTINEL_LOG="$RUN/sentinel.log" \
        command heimdall --home "$SH" "$@" 2>&1
}
fixture_bd() { timeout 5 bd -C "$SPIRA_DB" "$@"; } # STORE_DOOR_CONSIDERED="a fixture call against the suite's own throwaway store"
alert_status() {   # alert_status <key> -> "<id> <status>", or "-" if there is no bead
    fixture_bd list --all --limit 0 --label alert --json 2>/dev/null \
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
    fixture_bd list --all --limit 0 --label alert --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))'
}
n_probe_beads() {   # open + closed heimdall:probe beads
    fixture_bd list --all --limit 0 --label heimdall:probe --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(len(d if isinstance(d, list) else [d]))'
}
create_probe() {    # create_probe -> id on stdout
    fixture_bd create --title "Heimdall write probe" --type event -p 0 \
        --labels "heimdall:probe,overseer" \
        --body "write-path probe — updated on every Heimdall pass" --json 2>/dev/null \
        | python3 -c '
import sys, json
t = sys.stdin.read(); i = t.find("{")
if i >= 0:
    try: print(json.loads(t[i:])["id"])
    except Exception: pass' 2>/dev/null
}
# A REALISTIC WEDGE: passes completed normally until an hour ago, then kept starting and
# stopped finishing. A log with no completed pass ANYWHERE cannot distinguish a wedged
# loop from a box where Heimdall came up first, and the floor deliberately reads it as the
# second — so a fixture without completions would test the floor, not the wedge.
wedge()  { { mklog 6 5 1 1 1 "$(( $(date +%s) - 3600 ))"
             mklog 10 5 0 0 0 "$(date +%s)"; } > "$RUN/sentinel.log"; }
heal()   { mklog 10 5 1 1 1 "$(date +%s)" > "$RUN/sentinel.log"; }
