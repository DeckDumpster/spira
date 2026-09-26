#!/usr/bin/env bash
#
# test-cockpit-runtime-path.sh — cockpit answer-state paths default under SPIRA_RUN, not the cockpit dir.
#
# THE DEFECT. cockpit/answered-since.sh used RUNTIME="$(dirname "$0")/.runtime", so all mark
# files land inside the harness tree. Making the harness read-only (the release-unit goal)
# breaks writes. The fix moves them to SPIRA_RUN via dedicated conf.sh keys.
#
# WHAT IS TESTED (five conf.sh keys verified against SPIRA_RUN / ~/.config/spira):
#   SPIRA_ANSWER_STATE     — answered-seen.json; watch-answers.sh writes it, the health
#                            assertion reads it.
#   SPIRA_ANSWER_MARK      — verdict cursor for answered-since.sh.
#   SPIRA_ANSWER_COMMENT_MARK — comment cursor for answered-since.sh.
#   SPIRA_SELF_CLOSED      — beads the harness closed itself; filter for resolve.sh.
#   SPIRA_PREFIX_MAP       — operator-owned config that moves to ~/.config/spira.
#
# POSITIVE CONTROL FIRST. conf_val reads conf.sh and extracts a key. Before asserting that
# any key falls under SPIRA_RUN, assert that SPIRA_RUN itself has a non-empty default — an
# empty SPIRA_RUN would make every "starts with SPIRA_RUN" check vacuously true.
#
# tier: T1
# covers: spira/conf.sh cockpit/answered-since.sh cockpit/watch-answers.sh cockpit/resolve.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isne() { [ "$2" != "$3" ] && ok "$1" || bad "$1" "wanted NOT [$2] got [$3]"; }

echo "test-cockpit-runtime-path.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A minimal harness tree. conf.sh is symlinked so SPIRA_HOME resolves to $TMP/harness/spira
# (dirname of BASH_SOURCE[0]), and SPIRA_REPO resolves to $TMP/harness (the git-toplevel
# fallback: the parent, because $TMP has no git repository). SPIRA_RUN then defaults to
# $TMP/harness/.runtime/spira, which is distinct from the cockpit path $TMP/harness/cockpit.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"

# conf_two <key>: source conf.sh and print SPIRA_RUN<TAB><key-value>.
# Receives no SPIRA_HOME so conf.sh derives it from BASH_SOURCE[0] = $HARNESS/spira/conf.sh.
# HOME is a scratch dir so $HOME/.config is isolated from the real operator config.
conf_two() {
    local key="$1"; shift
    env -i "$@" PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        bash -c \
        ". '$HARNESS/spira/conf.sh'
         printf '%s\t%s' \"\${SPIRA_RUN:-}\" \"\${${key}:-}\"" 2>/dev/null
}

# ==========================================================================
echo
echo "positive control — SPIRA_RUN is non-empty (prevents vacuous prefix checks):"
# ==========================================================================
run_val="$(conf_two SPIRA_RUN | cut -f1)"
isne "SPIRA_RUN default is non-empty" "" "$run_val"

# ==========================================================================
echo
echo "positive control — the prefix check CAN detect a wrong value:"
# ==========================================================================
# If the check were broken (e.g., always passed), the test below would pass even without
# the fix. This verifies that a value NOT under SPIRA_RUN IS detected as wrong.
wrong_path="$HARNESS/cockpit/.runtime/answered-seen.json"
result_pc="$(conf_two SPIRA_ANSWER_STATE SPIRA_ANSWER_STATE="$wrong_path")"
run_pc="$(printf '%s' "$result_pc" | cut -f1)"
state_pc="$(printf '%s' "$result_pc" | cut -f2)"
case "$state_pc" in
    "$run_pc"/*) bad "positive control: value not under SPIRA_RUN IS detected" \
                     "prefix check PASSED for [$state_pc] under [$run_pc]" ;;
    *) ok "positive control: a value not under SPIRA_RUN is detected as wrong" ;;
esac

# ==========================================================================
echo
echo "SPIRA_ANSWER_STATE defaults under SPIRA_RUN:"
# ==========================================================================
result="$(conf_two SPIRA_ANSWER_STATE)"
run="$(printf '%s' "$result" | cut -f1)"
state="$(printf '%s' "$result" | cut -f2)"
case "$state" in
    "$run"/*) ok "SPIRA_ANSWER_STATE default is under SPIRA_RUN" ;;
    *) bad "SPIRA_ANSWER_STATE default is under SPIRA_RUN" \
           "got [$state], SPIRA_RUN=[$run]" ;;
esac

# ==========================================================================
echo
echo "SPIRA_ANSWER_MARK and SPIRA_ANSWER_COMMENT_MARK default under SPIRA_RUN:"
# ==========================================================================
for key in SPIRA_ANSWER_MARK SPIRA_ANSWER_COMMENT_MARK; do
    result="$(conf_two "$key")"
    run="$(printf '%s' "$result" | cut -f1)"
    val="$(printf '%s' "$result" | cut -f2)"
    case "$val" in
        "$run"/*) ok "$key default is under SPIRA_RUN" ;;
        *) bad "$key default is under SPIRA_RUN" \
               "got [$val], SPIRA_RUN=[$run]" ;;
    esac
done

# ==========================================================================
echo
echo "SPIRA_SELF_CLOSED defaults under SPIRA_RUN:"
# ==========================================================================
result="$(conf_two SPIRA_SELF_CLOSED)"
run="$(printf '%s' "$result" | cut -f1)"
val="$(printf '%s' "$result" | cut -f2)"
case "$val" in
    "$run"/*) ok "SPIRA_SELF_CLOSED default is under SPIRA_RUN" ;;
    *) bad "SPIRA_SELF_CLOSED default is under SPIRA_RUN" \
           "got [$val], SPIRA_RUN=[$run]" ;;
esac

# ==========================================================================
echo
echo "SPIRA_PREFIX_MAP defaults under ~/.config/spira, not inside SPIRA_HOME:"
# ==========================================================================
# POSITIVE CONTROL: a value baked inside SPIRA_HOME does NOT satisfy the .config/spira check.
# This proves the check is not vacuously passing everything.
harness_spira="$HARNESS/spira"  # this is what SPIRA_HOME resolves to
result_pm_wrong="$(conf_two SPIRA_PREFIX_MAP SPIRA_PREFIX_MAP="$harness_spira/prefix-map")"
val_pm_wrong="$(printf '%s' "$result_pm_wrong" | cut -f2)"
case "$val_pm_wrong" in
    *"/.config/spira/"*) bad "positive control: harness-dir path not excluded from .config/spira check" \
                             "check would PASS for [$val_pm_wrong]" ;;
    *) ok "positive control: harness-dir path correctly excluded from .config/spira check" ;;
esac

result_pm="$(conf_two SPIRA_PREFIX_MAP)"
val_pm="$(printf '%s' "$result_pm" | cut -f2)"
case "$val_pm" in
    *"/.config/spira/"*) ok "SPIRA_PREFIX_MAP default is under .config/spira" ;;
    *) bad "SPIRA_PREFIX_MAP default is under .config/spira" "got [$val_pm]" ;;
esac

echo
tl_summary
