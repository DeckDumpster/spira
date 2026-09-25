#!/usr/bin/env bash
#
# test-cockpit-layout-conf.sh — SPIRA_CONF, and the split-checkout renderer choice,
# propagate into the health pane's command.
#
# THE FAILURE THIS SUITE EXISTS FOR. layout.sh spawns health.sh in a tmux pane. tmux gives
# each new pane the server's environment, which is set once at server start and does not
# carry per-invocation env vars. An operator running `SPIRA_CONF=/test.conf layout.sh up`
# expects a test pane; without this fix the pane respawns with no SPIRA_CONF and reads the
# prod config instead — the wrong runtime tree, silently, with no visible error.
#
# DEMOTED TO T1: build_health_cmd() is the whole string layout.sh hands tmux for the health
# pane. Sourcing layout.sh and calling it directly needs no tmux server, no pane, no sleep —
# the four cases below used to drive a real fixture server and read #{pane_start_command}
# back off it (T2, ~4s); this reads the same string with no process spawned at all.
#
# covers: cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"
COCKPIT_DIR="$(dirname "$HERE")/cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

FAKE_COCK="$TMP/cockpit"; mkdir -p "$FAKE_COCK"
FAKE_PROD="$TMP/prodroot/cockpit"; mkdir -p "$FAKE_PROD"
FAKE_DEV_REPO="$TMP/devrepo"; mkdir -p "$FAKE_DEV_REPO"

# health_cmd <extra env...> — sources layout.sh in a pinned, empty environment (so no
# ambient spira.conf or SPIRA_PROD can decide the verdict) and prints build_health_cmd's
# output. TMUX_BIN is /bin/false: this never has to succeed, since nothing here calls it.
# SPIRA_CONF and SPIRA_PROD are NOT set here: each case states its own, including "absent".
health_cmd() {
    env -i HOME="$TMP" PATH="/usr/bin:/bin" \
        SPIRA_REPO="$TMP" SPIRA_COCKPIT="$FAKE_COCK" \
        SPIRA_RUN="$TMP/run" SPIRA_INSTANCE=fixture \
        SPIRA_LOOM_BIN="" COCKPIT_CWD="$TMP" COCKPIT_BOTTOM_PCT=30 COCKPIT_RIGHT_PCT=33 \
        COCKPIT_MAIL="" TMUX_BIN=/bin/false \
        "$@" \
        bash -c '. "'"$LAYOUT"'"; build_health_cmd'
}

echo "test-cockpit-layout-conf.sh"

echo
echo "SPIRA_CONF propagates into the health pane command:"
out1="$(health_cmd env SPIRA_CONF="$TMP/myinstance.conf" SPIRA_PROD="$TMP/noprod")"
want "carries SPIRA_CONF=" "SPIRA_CONF=" "$out1"
want "carries the conf path" "$TMP/myinstance.conf" "$out1"

echo
echo "no SPIRA_CONF: no prefix at all:"
out2="$(health_cmd env SPIRA_PROD="$TMP/noprod")"
nowant "no SPIRA_CONF prefix when unset" "SPIRA_CONF=" "$out2"
want "still runs this checkout's health.sh" "$FAKE_COCK/health.sh loop" "$out2"

echo
echo "split-checkout mode: SPIRA_PROD outside SPIRA_REPO selects the prod renderer:"
out3="$(health_cmd env SPIRA_REPO="$FAKE_DEV_REPO" SPIRA_PROD="$TMP/prodroot")"
want "uses SPIRA_PROD's cockpit" "$FAKE_PROD/health.sh" "$out3"
nowant "does not use the dev checkout's cockpit" "$FAKE_COCK/health.sh" "$out3"

echo
echo "SPIRA_DEV_RENDERER=1 opts back into the dev checkout even with SPIRA_PROD set:"
out4="$(health_cmd env SPIRA_REPO="$FAKE_DEV_REPO" SPIRA_PROD="$TMP/prodroot" SPIRA_DEV_RENDERER=1)"
want "keeps the dev checkout's renderer" "$FAKE_COCK/health.sh" "$out4"

printf '\ntest-cockpit-layout-conf: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
