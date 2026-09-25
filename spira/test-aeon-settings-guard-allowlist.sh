#!/usr/bin/env bash
#
# test-aeon-settings-guard-allowlist.sh — the --settings JSON aeon.sh writes for a
#   launched session names no PreToolUse hook outside a fixed allow-list.
#
# WHY THIS EXISTS
# ----------------
# sp-fjsxb retired the PreToolUse guard system: 7 of 10 guard scripts were wired
# nowhere and 2 more (bd-close-outcome-guard.sh, bd-delivers-label-guard.sh) were
# policy convention, not destructive-action prevention, so aeon_settings() in
# aeon.sh was trimmed to wire only what remains. Nothing stops a future edit from
# re-adding a pre_hooks.append(...) line the way the removed ones were added one
# bead at a time — this suite is what would catch that.
#
# ALLOW-LIST AND WHY EACH ENTRY REMAINS
# --------------------------------------
#   hooks/aeon-fence.sh        prevents pushes, queue operation, writes to the
#                               production checkout, and unmapped-repo/test-data
#                               bead creation — destructive actions with no
#                               structural replacement yet (sp-kz8ob, sp-mvg44).
#   bd-close-unacked-guard.sh  the unacked-close handling the bead named to keep.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the checker
# function must flag a disallowed PreToolUse command before its silence on the
# real output is trusted as evidence of compliance.
#
# tier: T1
# covers: spira/aeon.sh UC-safety-fences-16
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-aeon-settings-guard-allowlist.sh"

# check_allowlist <settings-json> -> prints one VIOLATION:<cmd> line per PreToolUse
# hook command whose basename is not in the allow-list; silent when clean.
check_allowlist() {
    python3 -c '
import json, os, sys

ALLOWED = {"aeon-fence.sh", "bd-close-unacked-guard.sh"}

try:
    d = json.loads(sys.argv[1])
except Exception as e:
    print("PARSE_ERROR:" + str(e))
    sys.exit(0)

pre = d.get("hooks", {}).get("PreToolUse", [])
for group in pre:
    for h in group.get("hooks", []):
        cmd = h.get("command", "")
        base = os.path.basename(cmd)
        if base not in ALLOWED:
            print("VIOLATION:" + base)
' "$1"
}

# ===========================================================================
echo
echo "POSITIVE CONTROL — checker flags a disallowed PreToolUse entry:"
# ===========================================================================
FAKE_JSON='{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"/opt/spira/bd-close-outcome-guard.sh"}]}]}}'
pc="$(check_allowlist "$FAKE_JSON")"
case "$pc" in
    VIOLATION:bd-close-outcome-guard.sh) ok "POSITIVE: checker flags a removed guard reintroduced into settings" ;;
    *) bad "POSITIVE: checker flags a removed guard reintroduced into settings" "got [$pc]" ;;
esac

# ===========================================================================
echo
echo "the real aeon_settings() output names no guard outside the allow-list:"
# ===========================================================================
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
SPIRA_HOME_FIXTURE="$TMP/home"; mkdir -p "$SPIRA_HOME_FIXTURE/hooks"
cp "$HERE/hooks/aeon-fence.sh" "$SPIRA_HOME_FIXTURE/hooks/aeon-fence.sh"
cp "$HERE/hooks/aeon-mail-deliver.sh" "$SPIRA_HOME_FIXTURE/hooks/aeon-mail-deliver.sh"
cp "$HERE/bd-close-unacked-guard.sh" "$SPIRA_HOME_FIXTURE/bd-close-unacked-guard.sh"
chmod +x "$SPIRA_HOME_FIXTURE/hooks/aeon-fence.sh" \
         "$SPIRA_HOME_FIXTURE/hooks/aeon-mail-deliver.sh" \
         "$SPIRA_HOME_FIXTURE/bd-close-unacked-guard.sh"

# aeon.sh has no argument for "just define functions" — the top-level script
# requires a fayth and dies without one — so lib.sh (which aeon_settings lives in,
# shared with the sweep call site) is sourced directly instead.
REAL_JSON="$(SPIRA_HOME="$SPIRA_HOME_FIXTURE" bash -c '
    . "$1/lib.sh"
    aeon_settings
' -- "$HERE")"

real_violations="$(check_allowlist "$REAL_JSON")"
if [ -z "$real_violations" ]; then
    ok "aeon_settings() PreToolUse hooks are within the allow-list"
else
    bad "aeon_settings() PreToolUse hooks are within the allow-list" "$real_violations"
fi

case "$REAL_JSON" in
    *aeon-fence.sh*) ok "allow-listed hook aeon-fence.sh still wired" ;;
    *) bad "allow-listed hook aeon-fence.sh still wired" "missing from [$REAL_JSON]" ;;
esac
case "$REAL_JSON" in
    *bd-close-unacked-guard.sh*) ok "allow-listed hook bd-close-unacked-guard.sh still wired" ;;
    *) bad "allow-listed hook bd-close-unacked-guard.sh still wired" "missing from [$REAL_JSON]" ;;
esac

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
