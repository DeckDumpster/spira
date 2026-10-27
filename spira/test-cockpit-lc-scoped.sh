#!/usr/bin/env bash
#
# test-cockpit-lc-scoped.sh — a cockpit-collect probe asks spira-lc only for the rows it
# renders (by id, state or hold), never the whole lifecycle store, and each answer stays small.
#
# The fixture store is larger than the 64 KiB a probe may draw per call; the mirror logs every
# `list` answer's size and arguments, so a probe that falls back to the unscoped list shows as
# an unscoped or oversized entry.
#
# tier: T2
# covers: cockpit-collect/src/* spira/testlib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

RUN="$TMP/run"; mkdir -p "$RUN"
LOG="$TMP/lc.log"
lc_mirror_bd "$TMP/lc"
tl_config SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
    SPIRA_RUNES=builder SPIRA_SCOPE_LABEL=spira SPIRA_ASK_LABEL=needs-ryan SPIRA_RUNES_DIR="$HERE/runes"

python3 -I - "$TMP/store.json" <<'PY'
import json, sys
beads = []
for n in range(1500):
    labels = ["spira", "plan", "incident"] + (["spira-poison"] if n % 500 == 7 else [])
    beads.append({"id": "sp-%04d" % n, "title": "bead %d" % n, "status": "open", "issue_type": "task",
                  "labels": labels, "external_ref": "ref-%d" % (n % 40), "created_at": "2026-10-01T00:00:00Z",
                  "updated_at": "2026-10-01T00:00:00Z"})
json.dump(beads, open(sys.argv[1], "w"))
PY

probe() {    # probe <name>
    env -i SPIRA_PANE_READ_TTL=0 SPIRA_HISTORY_READ_TTL=0 SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_MIRROR_LOG="$LOG" \
        PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_BDJSON_FIXTURE="$TMP/store.json" SPIRA_TOML="$SPIRA_TOML" \
        cockpit-collect probe "$1" 2>/dev/null
}

echo "test-cockpit-lc-scoped.sh"

full="$(env SPIRA_BDJSON_FIXTURE="$TMP/store.json" "$SPIRA_LC_BIN" list | wc -c)"
is "the fixture store's full list ($full bytes) exceeds 64 KiB" y "$([ "$full" -gt 65536 ] && echo y)"

for name in well reachable dup_refs czar_triggers; do
    : > "$LOG"
    out="$(probe "$name")"
    nowant "$name renders no ? for want of a lifecycle read" "=?" "$out"
    reads="$(wc -l < "$LOG")"
    is "$name read the lifecycle store" y "$([ "$reads" -gt 0 ] && echo y)"
    unscoped="$(awk '{ if ($0 !~ /--(ids|state|hold) /) print }' "$LOG" | wc -l)"
    is "$name made no unscoped list call" "0" "$unscoped"
    biggest="$(awk '{ if ($1 > m) m = $1 } END { print m + 0 }' "$LOG")"
    is "$name's largest list answer ($biggest bytes) is at most 64 KiB" y "$([ "$biggest" -le 65536 ] && echo y)"
done

: > "$LOG"
out="$(probe well)"
is "SP_POISON still counts the poison holds" "3" "$(printf '%s\n' "$out" | sed -n 's/^SP_POISON=//p')"
