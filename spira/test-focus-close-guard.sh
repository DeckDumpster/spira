#!/usr/bin/env bash
#
# test-focus-close-guard.sh — bd-close-focus-guard.sh refuses `bd close` without
#   a cited sp-id when a focus period (narrowed SPIRA_FAYTHS) is active in aeon
#   sessions, and stays quiet otherwise.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the guard MUST
# fire on the exact shape of the sp-7p9 incident before any negative control runs.
# Shape: focus period active (SPIRA_FAYTHS narrowed to one persona), close reason
# describes a finding but cites no bead id.
#
# law-a-regression-test-must-be-seen-to-fail: run this suite against the pre-fix
# tree (without bd-close-focus-guard.sh) and confirm the positive controls fail.
#
# WHAT IS VERIFIED
# 1. POSITIVE CONTROL: focus period + no sp-id in reason → guard fires (exits 2)
# 2. NEGATIVE CONTROL: focus period + sp-id of open bead → passes
# 3. NEGATIVE CONTROL: focus period + sp-id of insight bead → passes
# 4. POSITIVE CONTROL: focus period + sp-id of closed non-insight bead → guard fires
# 5. POSITIVE CONTROL (unfenced path): full roster (no focus), no sp-id → passes
# 6. Non-aeon sessions are not blocked
# 7. BD_CLOSE_FOCUS_OVERRIDE is honoured
# 8. Non-Bash tool calls pass through
# 9. bd update (not close) passes through
# 10. --reason flag form is also inspected
#
# covers: spira/bd-close-focus-guard.sh spira/lib.sh
# covers: spira/conf.sh
# covers: spira/chamber/*.fayth
# covers: spira/testdb.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant()  { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }
wantrc()  { if [ "$3" -eq "$2" ]; then ok "$1"; else bad "$1" "wanted rc=$2, got rc=$3"; fi; }

echo "test-focus-close-guard.sh"

GUARD="$HERE/bd-close-focus-guard.sh"
[ -f "$GUARD" ] || { printf 'SKIP bd-close-focus-guard.sh not found at %s\n' "$GUARD"; exit 77; }

# Verify the chamber has at least two auto-summoned personas. With only one, setting
# SPIRA_FAYTHS to that one persona would be the full roster, not a focus period.
auto_count=0
for _f in "$HERE/chamber"/*.fayth; do
    [ -e "$_f" ] || continue
    _summon=auto
    while IFS= read -r _line; do
        case "$_line" in FAYTH_SUMMON=*) _summon="${_line#FAYTH_SUMMON=}"; break ;; esac
    done < "$_f"
    [ "$_summon" = "auto" ] && auto_count=$((auto_count+1))
done
unset _f _summon _line
if [ "$auto_count" -lt 2 ]; then
    printf 'SKIP need at least 2 auto-summoned personas in chamber; found %d\n' "$auto_count"
    exit 77
fi

# Pick one auto-summoned persona name to use as the narrowed roster.
FOCUS_PERSONA=""
for _f in "$HERE/chamber"/*.fayth; do
    [ -e "$_f" ] || continue
    _summon=auto
    while IFS= read -r _line; do
        case "$_line" in FAYTH_SUMMON=*) _summon="${_line#FAYTH_SUMMON=}"; break ;; esac
    done < "$_f"
    if [ "$_summon" = "auto" ]; then
        FOCUS_PERSONA="$(basename "$_f" .fayth)"
        break
    fi
done
unset _f _summon _line
[ -n "$FOCUS_PERSONA" ] || { printf 'SKIP could not pick a focus persona\n'; exit 77; }

# Spin up a real throwaway database for sp-id verification tests.
. "$HERE/testdb.sh"
testdb_require fayth || exit 77
TMP="$(mktemp -d)"
trap 'testdb_drop 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
testdb_up fayth || exit 1
BD="${SPIRA_BD:-bd}"

# Create test beads in the fixture database.
# open_id: a bead in open (ready) state — a valid citation during a focus period.
open_id="$("$BD" -C "$SPIRA_DB" create --title "test open finding" -l spira --type task 2>/dev/null | grep -oE 'sp-[a-z0-9]+')"
[ -n "$open_id" ] || { printf 'SKIP could not create open bead in testdb\n'; exit 77; }

# insight_id: a closed bead with the insight label — also a valid citation.
insight_id="$("$BD" -C "$SPIRA_DB" create --title "test insight finding" -l insight --type task 2>/dev/null | grep -oE 'sp-[a-z0-9]+')"
[ -n "$insight_id" ] || { printf 'SKIP could not create insight bead in testdb\n'; exit 77; }
"$BD" -C "$SPIRA_DB" close "$insight_id" --reason "insight record" >/dev/null 2>&1

# closed_id: a bead that is closed but NOT an insight — an invalid citation.
closed_id="$("$BD" -C "$SPIRA_DB" create --title "test closed bead" -l spira --type task 2>/dev/null | grep -oE 'sp-[a-z0-9]+')"
[ -n "$closed_id" ] || { printf 'SKIP could not create closed bead in testdb\n'; exit 77; }
"$BD" -C "$SPIRA_DB" close "$closed_id" --reason "done" >/dev/null 2>&1

# A temporary conf file declaring a focus period: one persona out of the full roster.
FOCUS_CONF="$TMP/focus.conf"
printf 'SPIRA_FAYTHS = %s\n' "$FOCUS_PERSONA" > "$FOCUS_CONF"

# A temporary conf file declaring the full auto roster (not a focus period).
FULL_FAYTHS=""
for _f in "$HERE/chamber"/*.fayth; do
    [ -e "$_f" ] || continue
    _summon=auto
    while IFS= read -r _line; do
        case "$_line" in FAYTH_SUMMON=*) _summon="${_line#FAYTH_SUMMON=}"; break ;; esac
    done < "$_f"
    [ "$_summon" = "auto" ] && FULL_FAYTHS="$FULL_FAYTHS $(basename "$_f" .fayth)"
done
unset _f _summon _line
FULL_FAYTHS="${FULL_FAYTHS# }"
FULL_CONF="$TMP/full.conf"
printf 'SPIRA_FAYTHS = %s\n' "$FULL_FAYTHS" > "$FULL_CONF"

# make_payload <command> — produce the PreToolUse JSON for a Bash tool call.
make_payload() {
    python3 -c '
import json, sys
print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))
' "$1"
}

# run_guard <command> [KEY=VAL ...] -> combined stdout+stderr, returns guard exit code.
# Pipes the PreToolUse JSON to the guard with SPIRA_AEON=1 and FOCUS_CONF.
# SPIRA_HOME points to HERE (the harness spira/) so the chamber can be found.
run_guard() {
    local cmd="$1"; shift
    local rc out
    out=$(make_payload "$cmd" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_AEON=1 \
          SPIRA_HOME="$HERE" \
          SPIRA_CONF="$FOCUS_CONF" \
          SPIRA_DB="$SPIRA_DB" \
          SPIRA_BD="$BD" \
          "$@" \
          bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_full_roster: same but with the full roster (not a focus period).
run_guard_full_roster() {
    local cmd="$1"; shift
    local rc out
    out=$(make_payload "$cmd" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_AEON=1 \
          SPIRA_HOME="$HERE" \
          SPIRA_CONF="$FULL_CONF" \
          SPIRA_DB="$SPIRA_DB" \
          SPIRA_BD="$BD" \
          "$@" \
          bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# run_guard_no_aeon: same but WITHOUT SPIRA_AEON (maintenance / brain session).
run_guard_no_aeon() {
    local cmd="$1"; shift
    local rc out
    out=$(make_payload "$cmd" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_HOME="$HERE" \
          SPIRA_CONF="$FOCUS_CONF" \
          "$@" \
          bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# Commands for testing.
# BARE: the sp-7p9 shape — a finding mentioned in the reason but no bead id cited.
BARE="bd -C /tmp/db close sp-abc --reason-file - <<'REASON'
test-cockpit-ready.sh being red is a real finding — refile deliberately once the focus period closes
REASON"

OPEN_CITE="bd -C /tmp/db close sp-abc --reason-file - <<'REASON'
test-cockpit-ready.sh being red is a real finding — tracked in $open_id
REASON"

INSIGHT_CITE="bd -C /tmp/db close sp-abc --reason-file - <<'REASON'
test-cockpit-ready.sh being red is a real finding — recorded as $insight_id
REASON"

CLOSED_CITE="bd -C /tmp/db close sp-abc --reason-file - <<'REASON'
test-cockpit-ready.sh being red is a real finding — see $closed_id for context
REASON"

# ==========================================================================
echo
echo "POSITIVE CONTROL — sp-7p9 shape: focus period + finding + no bead id"
# ==========================================================================
# This is exactly the invocation that swallowed sp-7p9's finding. The guard MUST
# fire here; silence means it could never have caught that incident.

out="$(run_guard "$BARE" || true)"
want  "close with finding, no id refused in focus period"   "BLOCKED by bd-close-focus-guard" "$out"
want  "refusal names BD_CLOSE_FOCUS_OVERRIDE"               "BD_CLOSE_FOCUS_OVERRIDE"          "$out"
want  "refusal shows the narrowed persona"                  "$FOCUS_PERSONA"                   "$out"
rc=0; run_guard "$BARE" >/dev/null 2>&1 || rc=$?
wantrc "guard exits 2 to block the tool call"               2                                   "$rc"

# ==========================================================================
echo
echo "NEGATIVE CONTROL — same close with a cited open bead must pass"
# ==========================================================================

out="$(run_guard "$OPEN_CITE" || true)"
nowant "close citing open bead not blocked"                  "BLOCKED"                           "$out"
rc=0; run_guard "$OPEN_CITE" >/dev/null 2>&1 || rc=$?
wantrc "allowed close exits 0"                               0                                   "$rc"

# ==========================================================================
echo
echo "NEGATIVE CONTROL — same close with a cited insight bead must pass"
# ==========================================================================

out="$(run_guard "$INSIGHT_CITE" || true)"
nowant "close citing insight bead not blocked"               "BLOCKED"                           "$out"

# ==========================================================================
echo
echo "POSITIVE CONTROL — cited bead is closed and not an insight"
# ==========================================================================
# Citing a bead that is already closed (and is not an insight) does not satisfy
# the requirement: a closed bead cannot surface a new finding.

out="$(run_guard "$CLOSED_CITE" || true)"
want  "citing a closed non-insight bead is refused"          "BLOCKED by bd-close-focus-guard"  "$out"
want  "refusal identifies the invalid bead"                  "$closed_id"                        "$out"

# ==========================================================================
echo
echo "POSITIVE CONTROL (unfenced path) — full roster, no cited id: must pass"
# ==========================================================================
# The unfenced path (SPIRA_FAYTHS = full auto roster) accepts the bare-reason
# close. This proves the guard is the delta: the same close that would be blocked
# under a narrowed roster passes when the full roster is in force.

out="$(run_guard_full_roster "$BARE" || true)"
nowant "full roster: same close not blocked"                 "BLOCKED"                           "$out"
rc=0; run_guard_full_roster "$BARE" >/dev/null 2>&1 || rc=$?
wantrc "unfenced path exits 0"                               0                                   "$rc"

# ==========================================================================
echo
echo "non-aeon sessions are not blocked"
# ==========================================================================

out="$(run_guard_no_aeon "$BARE" || true)"
nowant "no SPIRA_AEON: not blocked"                          "BLOCKED"                           "$out"

# ==========================================================================
echo
echo "override is honoured"
# ==========================================================================

out="$(run_guard "$BARE" BD_CLOSE_FOCUS_OVERRIDE=1 || true)"
nowant "env override passes"                                 "BLOCKED"                           "$out"

out="$(run_guard "BD_CLOSE_FOCUS_OVERRIDE=1 $BARE" || true)"
nowant "inline override passes"                              "BLOCKED"                           "$out"

# ==========================================================================
echo
echo "non-Bash tools pass through"
# ==========================================================================

read_pl='{"tool_name": "Read", "tool_input": {"file_path": "/tmp/test.sh"}}'
out="$(printf '%s' "$read_pl" | \
  env -i PATH="$PATH" HOME="$HOME" \
      SPIRA_AEON=1 SPIRA_HOME="$HERE" SPIRA_CONF="$FOCUS_CONF" \
      bash "$GUARD" 2>&1 || true)"
nowant "non-Bash tool not blocked"                           "BLOCKED"                           "$out"

# ==========================================================================
echo
echo "bd update (not close) passes through"
# ==========================================================================

out="$(run_guard "bd -C /tmp/db update sp-abc --status in_progress" || true)"
nowant "bd update not blocked"                               "BLOCKED"                           "$out"

# ==========================================================================
echo
echo "--reason flag form is also inspected"
# ==========================================================================

out="$(run_guard 'bd close sp-abc --reason "real finding here, no bead cited"' || true)"
want  "--reason flag form detected and refused"              "BLOCKED by bd-close-focus-guard"   "$out"

out="$(run_guard "bd close sp-abc --reason \"finding tracked in $open_id\"" || true)"
nowant "--reason flag with sp-id not blocked"                "BLOCKED"                           "$out"

# ==========================================================================
echo
echo "single-quoted prose does not fire the guard"
# ==========================================================================

out="$(run_guard "echo 'bd close sp-abc --reason \"defect found, refile later\"'" || true)"
nowant "single-quoted prose not blocked"                     "BLOCKED"                           "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
