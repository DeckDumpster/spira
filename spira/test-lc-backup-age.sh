#!/usr/bin/env bash
# test-lc-backup-age.sh — lc-backup age is red when any listed database was never backed up,
# whichever position it holds. No store, no sleeps, no concurrent writers: stamp files only.
#
# tier: T1
# covers: spira/lc-backup.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup-age.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
RUN="$TMP/run"; mkdir -p "$RUN"
lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_LC_BACKUP_URL="file://$TMP/dest" \
        SPIRA_LC_BACKUP_MAX_AGE=1000 SPIRA_LC_BACKUP_ALLOW_LOCAL=1 SPIRA_LC_BACKUP_DB="$DBS")" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}

echo; echo "matcher — a never line anywhere is not all-ok:"
eval "$(sed -n '/^_lcb_all_ok()/p' "$HERE/lc-backup.sh")"
declare -F _lcb_all_ok >/dev/null || bail "_lcb_all_ok was not extracted from lc-backup.sh"
_lcb_all_ok "db0: ok 1s"; wantrc "an all-ok list is accepted" 0 $?
lines="$(yes 'db0: never' | head -20000)"
_lcb_all_ok "$lines"; wantrc "a 20000-line status whose first line is never is not all-ok" 1 $?

echo; echo "age — a never-backed-up database listed first:"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.fresh.ok"
DBS="fresh"; lcb age >/dev/null; wantrc "control: a lone fresh database is green" 0 $?
DBS="gap fresh"; out="$(lcb age)"; rc=$?
wantrc "a never-backed-up database listed first is red" 1 $rc; want "the first-listed one is named" "gap: never" "$out"
wide="$(for i in $(seq 1500); do printf 'pad%04d_%060d ' "$i" 0; done)"
DBS="gap fresh $wide"; out="$(lcb age)"; rc=$?
wantrc "listed first of 100KB of status it is still red" 1 $rc; want "and still named" "gap: never" "$out"

tl_summary
