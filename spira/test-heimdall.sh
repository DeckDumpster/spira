#!/usr/bin/env bash
#
# test-heimdall.sh — the raise/clear/re-raise lifecycle against a real bd, the operator's own close, and the file channel of last resort.
#
#   ./test-heimdall.sh
#
# The classifier half (`heimdall classify`, the pure function that turns observations into
# firing keys) is test-heimdall-classify.sh, which needs nothing but the heimdall binary and runs
# everywhere. This suite drives a real `bd` against a throwaway Dolt database, because a
# stub for bd has twice made correct callers look broken (law-prefer-the-real-dependency).
# With no server, this suite exits 77 — the automake skip convention, which gate-brain.sh
# names in the gate's output — but ONLY if everything that did run passed. A skip must
# never be able to swallow a failure.
#
# mklog (a synthetic sentinel log in lib.sh's log() format) is needed here too, for the
# wedge()/heal() fixtures below — it is duplicated from test-heimdall-classify.sh rather than
# shared, since testlib.sh suites are sourced standalone with no shared-helper convention.
#
# heimdall.sh is retired into the `heimdall` binary (sp-zpaq0, rewrite wave 5); this suite now
# drives that binary (`command heimdall --home "$SH"`) instead of `bash "$SH/heimdall.sh"`,
# exactly test-dwarf-sweep.sh's own `command dwarf --home ...` pattern for the same reason
# (the wrapper function below is itself named `heimdall`, shadowing the bare binary name).
# Three files share this setup: each heimdall pass costs about a second of real bd round trips,
# so the sections run as test-heimdall.sh, test-heimdall-probe.sh and test-heimdall-restart.sh,
# each on a fixture of its own.
# tier: T2
# covers: heimdall/src/* spira/testlib/heimdall-fixture.sh spira/conf.sh UC-ops-detection-remediation-19
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v heimdall >/dev/null 2>&1 \
    || { echo "test-heimdall: heimdall is not on PATH" >&2; exit 1; }


TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "test-heimdall.sh"

. "$HERE/testdb.sh"
if ! testdb_available; then
    printf 'SKIP test-heimdall: no bd engine available — the reconcile cases need a real bd.\n' \
        >&2
    printf '  embedded: install bd-embedded  server: testenv testdb\n' \
        >&2
    skip "no bd engine available"
fi
testdb_up heimdall || { echo "test-heimdall: could not build a fixture database"; exit 1; }

. "$HERE/testlib/heimdall-fixture.sh"

echo "heimdall.sh — one bead per cause, raised, cleared and re-raised:"

wedge
heimdall >/dev/null
[ "$(alert_status sentinel-stalled)" = "-" ] \
    && ok "one sighting does not fire — a condition must be confirmed" \
    || bad "confirm" "an alert was raised on the first sighting"
[ -r "$RUN/heimdall.status" ] && ok "the heartbeat is written even when nothing fires" \
    || bad "heartbeat" "no $RUN/heimdall.status after a quiet run"

heimdall >/dev/null
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

heimdall >/dev/null
[ "$(n_alert_beads)" = 1 ] && ok "a condition that keeps holding does not open a second bead" \
    || bad "duplicates" "$(n_alert_beads) alert beads after three runs of one condition"

heal; heimdall >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID open" ] \
    && ok "one clear sighting does not retract it" \
    || bad "clear hysteresis" "the alert was retracted on the first quiet run"
heimdall >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID closed" ] \
    && ok "Heimdall closes its own alert when the condition passes" \
    || bad "clear" "expected [$ID closed] got [$(alert_status sentinel-stalled)]"

wedge; heimdall >/dev/null; heimdall >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID open" ] \
    && ok "the condition returning reopens the SAME bead" \
    || bad "reopen" "expected [$ID open] got [$(alert_status sentinel-stalled)]"
[ "$(n_alert_beads)" = 1 ] && ok "a flap does not accumulate beads" \
    || bad "flap" "$(n_alert_beads) beads after one flap — a pane becomes wallpaper this way"
flaps="$(awk -F'\t' '$1=="sentinel-stalled"{print $7}' "$RUN/heimdall.state")"
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
# `silent-until:` is the pane's own affordance and Heimdall must never touch it, or a silence
# the operator asked for would evaporate on the next pass.
timeout 5 bd -C "$SPIRA_DB" update "$ID" --add-label acked >/dev/null 2>&1
timeout 5 bd -C "$SPIRA_DB" update "$ID" --add-label "silent-until:2099-01-01T00:00:00Z" >/dev/null 2>&1
heal; heimdall >/dev/null; heimdall >/dev/null       # clears
wedge; heimdall >/dev/null; heimdall >/dev/null      # and returns
lab="$(timeout 5 bd -C "$SPIRA_DB" show "$ID" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
ls = d.get("labels") or []
print("%s %s" % ("acked" in ls, any(l.startswith("silent-until:") for l in ls)))' 2>/dev/null)"
is "a returning condition clears acked and leaves silent-until alone (acked cleared, silence kept)" \
   "False True" "$lab"

echo
echo "heimdall.sh — the operator's own close, and the channel of last resort:"

# Closing it by hand is an acknowledgement. Re-raising it would be a machine arguing with
# the person it is reporting to. The operator's close is fixture state, declared as data
# (sp-voip5): what is under test is Heimdall's answer to a closed row, not bd's close verb.
testdb_restate "$ID" closed
sed -i 's/\t[0-9]*$/\t0/' "$RUN/heimdall.state"      # force the hourly refresh window open
heimdall >/dev/null; heimdall >/dev/null
[ "$(alert_status sentinel-stalled)" = "$ID closed" ] \
    && ok "an alert closed by hand is not reopened while it still fires" \
    || bad "acknowledge" "Heimdall reopened a bead the operator had closed"

HEIMDALL_DB=/nonexistent-spira-db heimdall >/dev/null
[ -r "$RUN/heimdall.alerts.json" ] && ok "an unreachable database falls back to the file channel" \
    || bad "fallback" "no fallback file was written with the database down"
fb="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print("%s %s" % (d["db_reachable"], ",".join(sorted(a["key"] for a in d["alerts"]))))' \
    "$RUN/heimdall.alerts.json" 2>/dev/null)"
case "$fb" in "False "*db-unreachable*) ok "the fallback carries the alerts and says the database is down" ;;
    *) bad "fallback content" "got [$fb]" ;; esac
grep -q 'SP_HEIMDALL_DB_READ=down' "$RUN/heimdall.status" \
    && ok "the heartbeat is written even when the database is down" \
    || bad "heartbeat under failure" "SP_HEIMDALL_DB_READ is not down in $RUN/heimdall.status"

heimdall >/dev/null
[ -e "$RUN/heimdall.alerts.json" ] \
    && bad "fallback removal" "the fallback file survived the database coming back — two sources of truth" \
    || ok "the fallback file is removed once beads answers again"

testdb_drop >/dev/null 2>&1
tl_summary
