#!/usr/bin/env bash
#
# test-lc-parity-capture.sh — the parity-golden capture tool (tools/lc-parity/capture.sh) gives
# the same goldens every time from the same testdb, and never copies the production store.
#
# A goldens file a port is compared against is only evidence if a second capture of the same
# store reproduces it byte for byte, and only safe if the tool cannot be pointed at the live
# lifecycle store: it refuses any path that is, contains or sits inside the store the config
# declares, and a store a server is running on.
#
# tier: T3
# covers: tools/lc-parity/* lifecycle/schema.sql spira-lc/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
CAPTURE="$HERE/../tools/lc-parity/capture.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export TMPDIR="$TMP"

# The real seed, with a few cases that between them write rows, run a cascade, and print a
# wall-clock-stamped log line.
cat > "$TMP/cases.sh" <<EOF
. "$HERE/../tools/lc-parity/cases.sh"
for f in \$(declare -F | awk '{print \$3}' | grep '^case_'); do
    case "\$f" in case_show_ready|case_claim|case_land|case_deliver_push|case_list_asks) ;; *) unset -f "\$f" ;; esac
done
EOF

capture() { bash "$CAPTURE" --cases "$TMP/cases.sh" "$@" 2>&1; }

echo
echo "the same testdb gives the same goldens:"
out="$(capture --save-seed "$TMP/seed" "$TMP/a")"; rc=$?
wantrc "a fresh capture succeeds" 0 "$rc"
want "...and says how much it captured" "steps in" "$out"
capture "$TMP/b" >/dev/null
wantrc "a second fresh capture succeeds" 0 $?
is "...with goldens identical to the first" "" "$(diff -r "$TMP/a" "$TMP/b")"
want "...including a call's stdout" '"state": "WORKING"' "$(cat "$TMP/a/claim/02.stdout")"
is "...and a log line's wall-clock stamp, written as a token" "@WALL@" "$(cut -d' ' -f1 "$TMP/a/deliver-push/01.stdout")"
is "the saved seed has no server running on it" "" "$(find "$TMP/seed" -name sql-server.lock)"
capture --source "$TMP/seed" "$TMP/c" >/dev/null
wantrc "a capture from a copied testdb succeeds" 0 $?
capture --source "$TMP/seed" "$TMP/d" >/dev/null
is "...and twice in a row on the same copy gives identical goldens" "" "$(diff -r "$TMP/c" "$TMP/d")"
is "...which match the fresh capture's" "" "$(diff -r -x _seed "$TMP/a" "$TMP/c")"

echo
echo "it never copies the production store:"
tl_config SPIRA_DOLT_DATA="$TMP/seed"
LIVE="$(realpath "$TMP/seed")"
for src in "$TMP/seed" "$TMP/seed/spira_lifecycle/.." "$TMP"; do
    out="$(capture --source "$src" "$TMP/out-refused")"; rc=$?
    wantrc "a source overlapping the configured store is refused ($src)" 2 "$rc"
    want "...naming the store" "overlaps the production store $LIVE" "$out"
done
capture --work "$TMP/seed/w" "$TMP/out-refused" >/dev/null
wantrc "a work directory inside it is refused" 2 $?
capture "$TMP/seed/out" >/dev/null
wantrc "an output directory inside it is refused" 2 $?
is "...and nothing was written" "" "$(ls -d "$TMP/out-refused" "$TMP/seed/out" "$TMP/seed/w" 2>/dev/null)"
mkdir -p "$TMP/linked"
ln -s "$TMP/seed/spira_lifecycle" "$TMP/linked/spira_lifecycle"
out="$(capture --source "$TMP/linked" "$TMP/out-refused")"; rc=$?
wantrc "a source whose store is a link to it is refused" 2 "$rc"
want "...saying so" "symbolic link" "$out"
tl_config SPIRA_DOLT_DATA="$TMP/elsewhere"
tl_config SPIRA_DOLT_DURABLE="$TMP/seed"
capture --source "$TMP/seed" "$TMP/out-refused" >/dev/null
wantrc "the configured durable copy is protected too" 2 $?
tl_config SPIRA_DOLT_DURABLE=""
out="$(SPIRA_TOML="$TMP/no-such.toml" bash "$CAPTURE" --source "$TMP/seed" "$TMP/out-refused" 2>&1)"; rc=$?
wantrc "a config that cannot be read is a refusal, not a default" 2 "$rc"
want "...saying why" "cannot read spira.dolt_data" "$out"

mkdir -p "$TMP/precious" && echo keep > "$TMP/precious/file"
capture "$TMP/precious" >/dev/null
wantrc "an OUT_DIR holding something else is not emptied" 2 $?
is "...and its file survives" "keep" "$(cat "$TMP/precious/file")"
capture "$TMP/a" >/dev/null
wantrc "an earlier capture is overwritten" 0 $?

cp -a "$TMP/seed" "$TMP/live"
: > "$TMP/live/spira_lifecycle/.dolt/sql-server.lock"
out="$(capture --source "$TMP/live" "$TMP/out-live")"; rc=$?
wantrc "a store a server is running on is refused" 2 "$rc"
want "...saying so" "a server is running" "$out"
out="$(env -u SPIRA_TOML bash "$CAPTURE" --source "$TMP/seed" "$TMP/out-noconf" 2>&1)"; rc=$?
wantrc "without a config naming the store, it refuses" 2 "$rc"
want "...saying why" "cannot tell where the production store is" "$out"

tl_summary
