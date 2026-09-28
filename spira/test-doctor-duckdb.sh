#!/usr/bin/env bash
#
# test-doctor-duckdb.sh — doctor.sh's doctor_check_duckdb: run/tsd/'s query layer (DuckDB)
# is FAIL when absent from PATH, not warn — a blind reconciler-flow (every invariant logging
# unobservable forever) must never pass for a quiet one (sp-ujhmm).
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. A PATH with no duckdb on it FAILs, naming duckdb and the consequence
#    (tsd-query.sh and reconciler-flow), before the passing case below is trusted.
# 2. PRESENT. A stubbed duckdb on PATH reads as ok — the check is not always-FAIL.
#
# tier: T1
# covers: spira/doctor.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-doctor-duckdb.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home/.local/bin" "$TMP/run"

# SPIRA_DUCKDB_BIN, not PATH: a testenv container bakes a real duckdb into /usr/local/bin
# (spira/testenv/Containerfile) — a directory conf.sh's own PATH construction always
# appends regardless of the caller's PATH (spira/conf.sh:1888) — so hiding duckdb from a
# fixture by controlling only $HOME/.local/bin (the trick test-doctor-operator-channel.sh
# uses for hunk) cannot simulate absence there. doctor_check_duckdb honours the same
# override reconciler-flow itself reads, so the fixture points it at a binary that is
# reliably absent instead.
run_doctor() {
    env -i \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_DOCTOR_INSTALLING=1 \
        SPIRA_DUCKDB_BIN="${SPIRA_DUCKDB_BIN_OVERRIDE:-duckdb}" \
        bash "$HERE/doctor.sh" 2>&1
}
tsd_section() { sed -n '/^time series query layer$/,/^$/p' <<< "$1"; }

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — no duckdb on PATH: FAILs, names the consequence:"
# ==========================================================================
SPIRA_DUCKDB_BIN_OVERRIDE="$TMP/no-such-duckdb-binary"
out1="$(run_doctor || true)"
sec1="$(tsd_section "$out1")"
want "positive control: FAIL fires" "FAIL" "$sec1"
want "positive control: names duckdb" "duckdb" "$sec1"
want "positive control: names tsd-query.sh" "tsd-query.sh" "$sec1"
want "positive control: names reconciler-flow" "reconciler-flow" "$sec1"

# ==========================================================================
echo
echo "2. PRESENT — a stubbed duckdb on PATH reads as ok:"
# ==========================================================================
printf '#!/bin/sh\necho "v0.0.0-stub"\n' > "$TMP/home/.local/bin/duckdb"
chmod +x "$TMP/home/.local/bin/duckdb"
SPIRA_DUCKDB_BIN_OVERRIDE="duckdb"
out2="$(run_doctor || true)"
sec2="$(tsd_section "$out2")"
want   "stub present: duckdb ok" "ok    duckdb" "$sec2"
nowant "stub present: no FAIL naming duckdb" "FAIL  duckdb" "$sec2"

echo
tl_summary
