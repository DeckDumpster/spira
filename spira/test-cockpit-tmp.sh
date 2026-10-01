#!/usr/bin/env bash
#
# test-cockpit-tmp.sh — the collector's temp file, seen surviving nothing.
#
#   ./test-cockpit-tmp.sh
#
# sp-kt4l3: the kill-mid-probe sections ("a killed pass leaves no temp behind", "an
# interrupted pass does not disturb the published snapshot") are retired. They depended on
# spira/cockpit.sh's write_snapshot creating its temp file via `mktemp` BEFORE running the
# probe pass and holding it open, redirected, for the pass's entire duration — the shape
# that let 303 orphaned temps accumulate from kills landing mid-probe. cockpit-collect's
# `once` computes the full pass in memory first and only then opens, writes and renames the
# temp — one tight sequence with no multi-second window a signal can land inside. The class
# of leak this suite caught is now structural (rung 4 of the ladder: impossible, not
# refused), and `fs::rename` being atomic on POSIX means a kill either lands before the
# rename (cockpit.env is untouched) or after it (the new snapshot is already complete) —
# there is no partially-written cockpit.env to observe either.
#
# What remains: sweep-temps still matters on its own terms — a previous process's crash
# between write and rename (or a leftover from before this binary existed) still leaves an
# orphan, and startup must still clear it.
#
# defect: sp-2yd
# covers: cockpit-collect/src/main.rs
# shellcheck disable=SC2034
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

RUN="$(mktemp -d)"
TMP="$(mktemp -d)"
trap 'rm -rf "$RUN" "$TMP"' EXIT
BASE_PATH="$PATH"

# temps -> how many .cockpit.* files are in the scratch run dir right now.
temps() { find "$RUN" -maxdepth 1 -name '.cockpit.*' | wc -l; }

echo "sweep_stale_tmps clears pre-existing orphaned temps at startup"
# Called synchronously as its own subcommand (cockpit-collect sweep-temps) — it runs to
# completion before that call returns, so the property is asserted directly, no poll, no kill.
touch "$RUN/.cockpit.99999" "$RUN/.cockpit.orphan"
[ "$(temps)" -eq 2 ] && ok "two orphaned temps present before the sweep (control)" \
                      || bad "expected 2 orphaned temps staged, found $(temps)"

env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_FAYTHS=t \
    SPIRA_COCKPIT="$TMP" \
    cockpit-collect sweep-temps >/dev/null 2>&1
rc=$?
[ "$rc" -eq 0 ] && ok "sweep-temps exited 0" || bad "sweep-temps exited $rc"

n="$(temps)"
[ "$n" -eq 0 ] && ok "sweep-temps removed both orphans" || bad "sweep-temps: $n orphan(s) left behind"

echo
tl_summary
