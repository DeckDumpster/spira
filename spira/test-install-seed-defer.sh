#!/usr/bin/env bash
#
# test-install-seed-defer.sh — install.sh seeds statutes only when the database can be reached,
#   and a re-install over an existing server-mode database whose server is down seeds them in
#   phase 4, after dolt-beads.service is up, instead of failing every statute in phase 3.
#
# THE DEFECT (acceptance phases B and D, 2026-09-26: 25 "FAILED law-*" lines). Phase 3 starts a
# temporary dolt server only when it has to run `bd init`. On a re-install over an existing
# database — any install after uninstall.sh, which stops dolt-beads — seed.sh ran against a
# stopped server, every statute failed ("circuit-breaker ... tripped"), and the failure was
# ignored because the database was not fresh: a statute a newer release adds was never seeded.
#
# _seed_when <fresh:0|1> <server-mode:0|1> <server-up:0|1> -> now | defer
#   now    a fresh database (phase 3 started dolt for bd init), embedded mode (no server), or a
#          server that answers
#   defer  an existing server-mode database whose server is down — seed after phase 4 starts it
#
# tier: T1
# covers: install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-seed-defer.sh"

# install.sh sets its own HERE (the repo root) when sourced; keep the path to it first.
INSTALL_SH="$HERE/../install.sh"
# shellcheck disable=SC1091
. "$INSTALL_SH"
declare -f _seed_when >/dev/null \
    || { bad "install.sh defines _seed_when" "absent"; tl_summary; exit 1; }

is "fresh database (phase 3 started dolt for bd init) seeds now"   now   "$(_seed_when 1 1 0)"
is "embedded mode (no server to be down) seeds now"                now   "$(_seed_when 0 0 0)"
is "existing server-mode database, server up, seeds now"           now   "$(_seed_when 0 1 1)"
is "existing server-mode database, server DOWN, defers to phase 4" defer "$(_seed_when 0 1 0)"

# THE WIRING: phase 4 runs the deferred seed after it has proven the store accepts connections,
# and a failure then is not ignored (the server is up; a failure is real).
_src="$(cat "$INSTALL_SH")"
want "phase 3 records a deferred seed"                  '_seed_deferred=1' "$_src"
want "phase 4 runs the deferred seed"                   'seeding statutes (deferred from phase 3' "$_src"
want "a deferred seed that fails again fails the phase" 'seed.sh failed with the database server running' "$_src"

tl_summary
