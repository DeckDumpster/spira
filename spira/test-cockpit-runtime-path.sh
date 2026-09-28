#!/usr/bin/env bash
#
# test-cockpit-runtime-path.sh — operator-owned config defaults under ~/.config/spira, not
# baked inside the harness tree.
#
# THE DEFECT THIS ONCE CAUGHT. Two answer-tracking cursors and a self-closed filter lived
# under this name; all three, and the scripts that read them, were deleted with the
# bd-scanning answer readers (sp-xsl8i) — operator answers now reach the concierge as mail,
# with no cursor to default anywhere. SPIRA_PREFIX_MAP is what remains: a value the operator
# sets themselves, which must resolve outside the (potentially read-only) harness checkout.
#
# tier: T1
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-cockpit-runtime-path.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A minimal harness tree. conf.sh is symlinked so SPIRA_HOME resolves to $TMP/harness/spira
# (dirname of BASH_SOURCE[0]), and SPIRA_REPO resolves to $TMP/harness (the git-toplevel
# fallback: the parent, because $TMP has no git repository).
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
