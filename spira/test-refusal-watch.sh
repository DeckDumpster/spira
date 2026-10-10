#!/usr/bin/env bash
#
# test-refusal-watch.sh — refused bead events are grouped by (actor, event, from_state, refusal)
# and a class over its baseline is filed once per repeat window, against a real Dolt seeded with
# an event table: a planted refusal class is reported, a class at baseline and refusals outside
# the window are not, and a second pass the same day files nothing.
#
# host-reason: starts its own disposable `dolt sql-server`, same shape as test-ops-read-model.sh.
#
# tier: T2
# covers: refusal-watch/* spira-lc/src/ops.rs spira-lc/src/main.rs systemd/spira-refusal-watch.* install/src/manifest.rs
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/lc-fixture.sh"

TMP="$(mktemp -d)"
trap 'lcfix_down; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || bail "could not build a lifecycle fixture"
command -v refusal-watch >/dev/null 2>&1 || bail "refusal-watch is not on PATH"

NOW="$(date +%s)"
python3 - "$NOW" > "$TMP/seed.sql" <<'PY'
import sys
now = int(sys.argv[1])
def ev(bead, event, frm, refusal, actor, at):
    return "('bead','%s','%s','%s','%s','%s',0,'%s','{}','%s',%d)" % (bead, event, frm, frm, frm, refusal, actor, at)
rows = []
for i in range(9):
    rows.append(ev("sp-p%d" % i, "Submit", "WORKING", "stale_version", "aeon:planted", now - 600 - i))
for i in range(5):
    rows.append(ev("sp-q%d" % i, "Claim", "READY", "wrong_state", "aeon:atbaseline", now - 300 - i))
for i in range(20):
    rows.append(ev("sp-o%d" % i, "Drop", "OPEN", "wrong_state", "aeon:ancient", now - 3 * 86400 - i))
rows.append("('bead','sp-ok','Claim','READY','READY','WORKING',1,NULL,'{}','aeon:fine',%d)" % (now - 100))
print("INSERT INTO event (machine,lc_key,event,expect,from_state,to_state,applied,refusal,evidence,actor,at) VALUES")
print(",\n".join(rows) + ";")
PY
lcfix_sql < "$TMP/seed.sql" >/dev/null 2>"$TMP/seed.err" || bail "seed failed: $(head -c 400 "$TMP/seed.err")"

raw="$(spira-lc ops-refusals 86400)"
count_of() { python3 -I -c 'import json,sys; d=json.load(sys.stdin); print(sum(int(c["n"]) for c in d["classes"] if c["actor"]==sys.argv[1]))' "$1" <<<"$raw"; }
is "the planted caller's nine refusals are one counted class" 9 "$(count_of aeon:planted)"
is "an applied event is not a refusal" 0 "$(count_of aeon:fine)"
is "refusals outside the window are not counted" 0 "$(count_of aeon:ancient)"
is "the newest examples come back with their times" "sp-p0@$((NOW - 600))" "$(python3 -I -c 'import json,sys; d=json.load(sys.stdin); print([c for c in d["classes"] if c["actor"]=="aeon:planted"][0]["examples"].split(",")[0])' <<<"$raw")"
spira-lc ops-refusals 0 >/dev/null 2>&1
wantrc "a zero window is refused" 2 $?

cat > "$TMP/filer" <<SH
#!/bin/sh
{ printf 'TITLE %s\n' "\$2"; cat; } >> "$TMP/filed"
SH
chmod +x "$TMP/filer"
export REFUSAL_WATCH_FILER="$TMP/filer"

refusal-watch pass --state "$TMP/state" >"$TMP/out" 2>&1
wantrc "a pass exits 0" 0 $?
filed="$(cat "$TMP/filed" 2>/dev/null)"
want "positive control: the planted class is filed" "TITLE refused Submit from WORKING (stale_version) by aeon:planted" "$filed"
want "the filing names the caller and an example bead" "Caller: aeon:planted" "$filed"
want "the filing carries an example bead with its time" "- sp-p0 at " "$filed"
nowant "a class exactly at baseline is not filed" "atbaseline" "$filed"
nowant "refusals outside the window are not filed" "ancient" "$filed"
is "exactly one bead is filed" 1 "$(grep -c '^TITLE ' "$TMP/filed")"

refusal-watch pass --state "$TMP/state" >/dev/null 2>&1
is "a second pass the same day files nothing more" 1 "$(grep -c '^TITLE ' "$TMP/filed")"

tl_summary
