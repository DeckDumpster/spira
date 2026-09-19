#!/usr/bin/env bash
# czar-fence.sh <class> — refuse queue mutations when the class is in shadow.
# Exit 0 in act; exit 1 in shadow (the default when unset).
#
# The class is the SPIRA_INCIDENT_CAUSE the trigger bead carries: e.g. deadlock,
# attribution-failed, loop-stalled. The stage var is SPIRA_CZAR_STAGE_<CLASS>
# with hyphens converted to underscores and lowercased → uppercased.
#
# law-guard-binds-the-caller: this fence binds the czar. Every queue mutation
# (eject, rerun, requeue, abandon, recertify, reopen) must pass through it.
# The default is shadow so a new class is safely contained until the operator
# sets SPIRA_CZAR_STAGE_<CLASS>=act in spira.conf.
set -uo pipefail

class="${1:-}"
[ -n "$class" ] || { printf 'czar-fence: class required\n' >&2; exit 2; }

var="SPIRA_CZAR_STAGE_$(printf '%s' "$class" | tr '[:lower:]-' '[:upper:]_')"
stage="${!var:-shadow}"

case "$stage" in
    act)    exit 0 ;;
    shadow) printf 'czar-fence: %s is shadow — mutation refused (%s=act to enable)\n' \
                "$class" "$var" >&2; exit 1 ;;
    *)      printf 'czar-fence: unknown stage %s in %s (shadow or act)\n' \
                "$stage" "$var" >&2; exit 2 ;;
esac
