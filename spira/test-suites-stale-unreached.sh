#!/usr/bin/env bash
# covers: spira/suites.sh
#
# sp-5hk2v: the "timed suites not reached last pass" field counted `<suite>.unreached`
# marker files under SPIRA_SUITES_STATE. No writer of that convention exists any more —
# `suites.sh run`, the only thing that ever wrote it, was retired outright (sp-b99nj) — so
# any marker on disk is unbounded debris from before the retirement, never cleared, and the
# field reported it as a fact about "last pass" (law-absence-needs-a-positive-control cited
# for a control nothing produces any more).
#
# This proves stale `.unreached` debris sitting in STATE has NO effect on `suites.sh status`
# or `suites.sh list`: the field naming it is gone, and a suite with a real, fresh `.result`
# is reported from that result alone, regardless of a marker sitting next to it.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-suites-stale-unreached.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

_sh="$TMP/spira"
_state="$TMP/state"
_home="$TMP/home"
mkdir -p "$_sh" "$_state" "$_home"

for _f in suites.sh lib.sh conf.sh suite-state.sh suite-covers.sh; do
    cp "$HERE/$_f" "$_sh/"
done

# Empty gate-suites: both stubs below are timed.
: > "$_sh/gate-suites"

printf '#!/usr/bin/env bash\n: # stub\n' > "$_sh/test-with-result.sh"
printf '#!/usr/bin/env bash\n: # stub\n' > "$_sh/test-never-run.sh"
chmod +x "$_sh/test-with-result.sh" "$_sh/test-never-run.sh"

# ---------------------------------------------------------------------------
# POSITIVE CONTROL — before planting anything, confirm the fixture actually
# drives cmd_status's counters: with no records and no markers at all, both
# stubs must show up as "no result yet".
# ---------------------------------------------------------------------------
run_status() {
    env -i PATH="$PATH" HOME="$_home" SPIRA_CONF="$TMP/no.conf" SPIRA_TOML="$TMP/no.toml" \
        SPIRA_HOME="$_sh" SPIRA_RUN="$TMP/run" SPIRA_SUITES_STATE="$_state" \
        bash "$_sh/suites.sh" status 2>&1
}
run_list() {
    env -i PATH="$PATH" HOME="$_home" SPIRA_CONF="$TMP/no.conf" SPIRA_TOML="$TMP/no.toml" \
        SPIRA_HOME="$_sh" SPIRA_RUN="$TMP/run" SPIRA_SUITES_STATE="$_state" \
        bash "$_sh/suites.sh" list 2>&1
}

_baseline="$(run_status)"
want "positive control: both stubs count as never-run before any fixture is planted" \
    "timed suites with no result yet     2" "$_baseline"

# ---------------------------------------------------------------------------
# FIXTURE — test-with-result.sh has a fresh, real .result AND a stale
# .unreached marker sitting next to it, exactly the coexistence the disk
# census in sp-5hk2v found (611 markers, the freshest 18h old, real results
# among them). test-never-run.sh has neither.
# ---------------------------------------------------------------------------
printf 'ok %s 1 -\n' "$(date +%s)" > "$_state/test-with-result.sh.result"
printf '%s\n' "$(( $(date +%s) - 200000 ))" > "$_state/test-with-result.sh.unreached"

out_status="$(run_status)"
out_list="$(run_list)"

nowant "status: the removed field never appears" "not reached last pass" "$out_status"
nowant "list: the removed REACH column never appears" "REACH" "$out_list"

# The suite with a real result is not counted as never-run just because a stale
# marker sits beside it — only the suite with no .result at all is.
want "status: only the suite with no .result counts as never-run" \
    "timed suites with no result yet     1" "$out_status"

want "list: the suite with a real result reports its own status, not the marker's" \
    "test-with-result.sh" "$out_list"
_row="$(printf '%s\n' "$out_list" | grep '^test-with-result.sh ')"
want "list: that row shows its .result status (ok)" "ok" "$_row"

tl_summary
