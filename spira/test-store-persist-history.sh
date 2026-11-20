#!/usr/bin/env bash
# tier: T2
# covers: store-persist/src/history.rs store-persist/src/lib.rs store-persist/tests/history_real_dolt.rs spira/conf.d/SPIRA_DOLT_PERSIST_HISTORY_KEEP
#
# The history bound against a real Dolt database: the squash keeps keep+1 commits and the same
# data, catches up commits made mid-rewrite, and the store fits the depth curve. A model of
# Dolt's rebase cannot stand in.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$HERE/.." || exit 1
out="$(cargo test -p store-persist --features integration --test history_real_dolt 2>&1)"; rc=$?
echo "$out" | tail -15
exit $rc
