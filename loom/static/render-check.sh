#!/usr/bin/env bash
#
# render-check.sh — drive the shipped page in a real browser and assert it painted.
#
#   loom/static/render-check.sh [payload.json]
#
# It serves this directory on a scratch port, loads every view in a headless browser, and
# checks the DOM afterwards. With no argument it uses `fixture.json`; pass a file holding a
# raw bead array to look at your own data.
#
# WHY THIS IS NOT A `spira/test-*.sh`. That glob is the population `suites.sh list` enumerates
# and the landing gate partly runs, and a browser is not something a clone — or a gate
# container — has any reason to have. A suite that skips on most machines trains everyone to
# ignore its skip line, which costs more than the coverage is worth. `spira/test-loom-page.sh`
# holds the half that needs no browser: the view model, and the structural properties of the
# page. This is the other half, run by hand when the page changes.
#
# EVERY ASSERTION HERE MUST BE UNSATISFIABLE BY THE STATIC MARKUP, and that is the whole
# lesson of the file. The first version looked for `<circle`, which the legend supplies
# whether or not anything rendered — so it reported six greens over a page whose render call
# had been dropped in a port. Assert on values only the painter can produce: a solved radius,
# a measured ink fraction, a tab whose selection was moved, a list built from beads.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
PAYLOAD="${1:-fixture.json}"

# A browser on PATH first, then the cache a browser-automation install uses. Named
# explicitly so a missing browser reports as a missing browser rather than as a page that
# will not paint.
BROWSER=""
for c in chromium chromium-browser google-chrome google-chrome-stable chrome; do
    command -v "$c" >/dev/null 2>&1 && { BROWSER="$(command -v "$c")"; break; }
done
[ -n "$BROWSER" ] || BROWSER="$(find "${HOME:-/nonexistent}/.cache/ms-playwright" -type f \
    \( -name 'chrome-headless-shell' -o -name 'chrome' \) 2>/dev/null | sort | tail -1)"
if [ -z "$BROWSER" ] || [ ! -x "$BROWSER" ]; then
    echo "SKIP render-check: no chromium-family browser found." >&2
    echo "     The page was NOT rendered; test-loom-page.sh covers the model, not the painting." >&2
    exit 2
fi
command -v python3 >/dev/null 2>&1 || { echo "render-check needs python3 to serve the page" >&2; exit 2; }

TMP="$(mktemp -d)"
PORT=0
cleanup() {
    [ -n "${SRV:-}" ] && kill "$SRV" 2>/dev/null
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

# An ephemeral port, chosen by the kernel and read back, so two runs cannot collide and a
# fixed port cannot be already taken by something else on the box.
PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$HERE" >"$TMP/srv.log" 2>&1 &
SRV=$!
for _ in $(seq 1 50); do
    curl -fsS -o /dev/null "http://127.0.0.1:$PORT/loom.html" 2>/dev/null && break
    sleep 0.1
done
curl -fsS -o /dev/null "http://127.0.0.1:$PORT/loom.html" || {
    echo "render-check: the scratch server never came up" >&2; exit 2; }

BASE="http://127.0.0.1:$PORT/loom.html?api=$PAYLOAD"
pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        %s\n' "$1" "$2"; fail=$((fail+1)); }

# EACH RUN GETS ITS OWN PROFILE. Sharing one user-data-dir across back-to-back launches makes
# some of them exit having written nothing, and an empty dump fails every assertion at once —
# which reads as "the page is broken" rather than "the check is".
N=0
dump() {
    N=$((N + 1))
    "$BROWSER" --headless --disable-gpu --no-sandbox --hide-scrollbars \
        --user-data-dir="$TMP/profile$N" --virtual-time-budget=20000 --dump-dom "$1" 2>/dev/null
}

want() {  # want <name> <dom> <ere>
    # AN EMPTY DUMP IS A BROKEN CHECK, NOT A BROKEN PAGE, and must never be reported as a
    # finding about the page (law-absence-needs-a-positive-control).
    [ ${#2} -gt 2000 ] || { bad "$1" "the dump is empty — the check is broken, not the page"; return; }
    # NEVER `printf ... | grep -q` UNDER pipefail: grep -q exits on the first match and closes
    # the pipe, the writer dies of SIGPIPE and pipefail propagates 141, so the check fails
    # precisely when it succeeds — intermittently, which reads as a flaky page.
    if grep -qE "$3" <<< "$2"; then ok "$1"; else bad "$1" "no match for /$3/"; fi
}

echo "map"
D="$(dump "$BASE#view=map")"
want "the boot overlay is gone"          "$D" 'class="boot hidden"'
want "the tab bar was repainted"         "$D" 'data-view="map" aria-selected="true"'
want "the stage svg was filled"          "$D" '<svg class="map" id="svgmap"[^>]*><'
want "blocks are labelled from beads"    "$D" 'class="rlab"'
want "the radius was solved"             "$D" 'id="d-r">[0-9]'
want "ink was measured, not assumed"     "$D" 'id="d-fillv"[^>]*>[0-9]+%'
want "the read cost is this load's own"  "$D" 'id="cost">[0-9]+ beads [^<]*derived [0-9]+ms'
want "vitals came from the model"        "$D" 'id="v-live">[0-9]+<'
want "p50 is in flight, not cycle time"  "$D" 'p50 in flight'

echo "chains"
D="$(dump "$BASE#view=chains")"
want "chains is the selected tab"        "$D" 'data-view="chains" aria-selected="true"'
want "components were drawn"             "$D" 'class="card chain"'
want "the head line counts them"         "$D" 'id="chainhead"[^>]*>[0-9]'

echo "churn"
D="$(dump "$BASE#view=churn")"
want "churn is the selected tab"         "$D" 'data-view="churn" aria-selected="true"'
want "attempt tracks were drawn"         "$D" 'class="track"'
want "the threshold marker was placed"   "$D" 'class="thresh"'

echo "flow"
D="$(dump "$BASE#view=flow")"
want "flow is the selected tab"          "$D" 'data-view="flow" aria-selected="true"'
want "arrivals were drawn"               "$D" 'title="[0-9]+ arrived"'
want "time in flight replaced cycle"     "$D" 'Time in flight'
want "the absent panel says why"         "$D" 'no closed bead is'
want "what moved is listed"              "$D" 'class="movedlist"'

echo "build"
D="$(dump "$BASE#view=build")"
want "build is the selected tab"         "$D" 'data-view="build" aria-selected="true"'

echo "state restores from the url"
D="$(dump "$BASE#view=map&edges=all")"
want "an edges mode round-trips"         "$D" 'data-edges="all" aria-pressed="true"'

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
