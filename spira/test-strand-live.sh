#!/usr/bin/env bash
#
# test-strand-live.sh — `strand report` counts live dwarves from the units systemd lists (the
#   source dwarves.sh and the runtime use), names them, reports no starvation while one is
#   working, and refuses rather than report zero when that source cannot be read.
#
# tier: T1
# covers: strand/src/* spira-world/src/* UC-ops-detection-remediation-21
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run" "$TMP/stubs/runes"
for _s in lib.sh conf.sh suite-covers.sh conf.d conf.toml.d; do ln -sfn "$HERE/$_s" "$TMP/stubs/$_s"; done

echo "test-strand-live.sh"

cat > "$TMP/stubs/runes/test-watcher.rune" <<'RUNE'
RUNE_NAME=test-watcher
RUNE_LABELS=spira,test-plan
RUNE_EXCLUDE_LABELS=spira-poison,needs-operator
RUNE_MAX_CONCURRENT=1
RUNE_TIMEOUT_SECONDS=3600
RUNE
tl_config SPIRA_RUNES_DIR="$TMP/stubs/runes"

cat > "$TMP/mock-bd" <<'MOCKBD'
#!/usr/bin/env bash
case "${1:-}" in -C) shift 2 ;; esac
case "${1:-}" in
    list)  printf '[{"id":"sp-t1","title":"test bead","status":"open","issue_type":"task","labels":["spira","test-plan"]}]\n' ;;
    *)     exit 0 ;;
esac
MOCKBD
chmod +x "$TMP/mock-bd"
lc_mirror_bd "$TMP/lc"

tl_config SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/no-db" SPIRA_BD="$TMP/mock-bd"
UNIT="spira-run-sentinel_2dcall-test_2dwatcher_2d1_2d0.service"
cat > "$TMP/systemctl-live" <<SC
#!/bin/sh
case "\$*" in *list-units*) echo "$UNIT loaded active running dwarf" ;; esac
SC
cat > "$TMP/systemctl-dead" <<'SC'
#!/bin/sh
echo "Failed to connect to bus" >&2
exit 1
SC
chmod +x "$TMP/systemctl-live" "$TMP/systemctl-dead"
mkdir -p "$TMP/run/task-ticked"; : > "$TMP/run/task-ticked/sentinel-pass"

report() {
    SPIRA_HOME="$TMP/stubs" PATH="$TMP/stubs:$TMP/lc:$PATH" SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/no-db" SPIRA_BD="$TMP/mock-bd" SPIRA_CALL=runtime \
    SPIRA_LABELS="spira,test-plan" SPIRA_SYSTEMCTL="$1" strand report 2>"$TMP/err"
}

echo
echo "case 1 — a live dwarf is named and the partition is not starved:"
out="$(report "$TMP/systemctl-live")"; cat "$TMP/err" >&2
want   "names the live dwarf"            "$UNIT"                 "$out"
want   "reports the scheduler's pass"    "scheduler last pass:"  "$out"
nowant "no starvation"                   "starved"               "$out"
nowant "no sentinel timer line"          "sentinel timer"        "$out"

echo
echo "case 2 — positive control: with no live dwarf the same fixture is starved:"
printf '#!/bin/sh\nexit 0\n' > "$TMP/systemctl-none"; chmod +x "$TMP/systemctl-none"
out="$(report "$TMP/systemctl-none")"
want   "starved when nothing is live"    "starved"               "$out"
nowant "starved advice names no timer"   "spira-sentinel.timer"  "$out"

echo
echo "case 3 — the live-dwarf source unreadable: refuse, never report zero:"
out="$(report "$TMP/systemctl-dead")"; rc=$?
wantrc "exits non-zero"                  2                       "$rc"
want   "says the dwarves are unknown"    "live dwarves are unknown" "$(cat "$TMP/err")"
nowant "no report printed"               "live dwarves:"         "$out"

tl_summary
