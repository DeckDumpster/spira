#!/usr/bin/env bash
# czar-fence.sh <class> — refuse queue mutations when the class is in shadow.
# Exit 0 in act; exit 1 in shadow (the default when unset); exit 2 with no class or an
# unknown stage.
#
# The class is the SPIRA_INCIDENT_CAUSE the trigger bead carries: e.g. deadlock,
# attribution-failed, loop-stalled. The stage var is SPIRA_CZAR_STAGE_<CLASS>
# with hyphens converted to underscores and lowercased → uppercased.
#
# law-guard-binds-the-caller: this fence binds the czar. Every queue mutation
# (eject, rerun, requeue, abandon, recertify, reopen) must pass through it.
# The default is shadow so a new class is safely contained until the operator
# sets SPIRA_CZAR_STAGE_<CLASS>=act in spira.conf.
#
# A thin caller: the fence is `bead::guard::czar_stage` (bead/src/guard.rs), the one
# implementation bdq and the content door (spira-lc content) also apply. Kept at this path
# because czar.rune's RUNE_TOOLS names it. It reads the stage from this process's own
# environment, as it always did (no conf.sh). `bead` is resolved through `command -v`, never a
# bare `exec bead` (bash's exec falls back to a CWD-relative lookup and would find the bead/
# crate directory at the checkout root). No bead binary is a refusal, never a pass.
set -uo pipefail

bin="$(command -v bead)" || { printf 'czar-fence: bead is not on PATH — mutation refused\n' >&2; exit 2; }
exec "$bin" czar-fence "${1:-}"
