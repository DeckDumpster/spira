#!/usr/bin/env bash
#
# test-cockpit-ready.sh — SP_READY, SP_NEXT_N, and SP_WAITING render ? when bd refuses.
#
#   ./test-cockpit-ready.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. bd exits 0 on a schema-version mismatch and
# prints the complaint to stdout, not stderr. Any probe that counts lines or checks $?
# reads the refusal as a successful empty response of zero. During the 2026-09-08
# outage, the cockpit displayed "SP_READY 0" and "SP_NEXT_N 0" while bd could not
# read the database at all, displacing the suspicion that would have prompted a look.
#
# defect: sp-vmh4
# tier: T1
# covers: spira/cockpit.sh
# scar: bd exits 0 on a schema-version mismatch and emits the refusal to stdout; a probe counting output lines read the refusal as zero, and SP_READY showed 0 during a real outage.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

run_probe() {   # run_probe <SPIRA_BD=path> -> stdout of probe()
    local bd_path="$1"
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=builder \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_BD="$bd_path" \
        bash "$HERE/cockpit.sh" once 2>/dev/null
}

# SPIRA_FAYTHS NAMES A PERSONA THAT EXISTS. It used to say `t`, for which there is no
# chamber/t.fayth — harmless only because the partition map globbed chamber/*.fayth and
# ignored SPIRA_FAYTHS altogether. Resolving partitions through fayth_get honours it, so an
# unresolvable persona now yields no partitions, which is a REFUSAL: the probe renders ?
# rather than 0. That is correct behaviour and it would have made this suite assert the
# opposite of what it means, so the fixture names a real persona instead.
#
# ======================================================================================
# POSITIVE CONTROL FIRST. A check that only tests absence is indistinguishable from one
# pointed at the wrong thing; proving it fires on a known input proves the machinery
# works before we rely on its silence (law-absence-needs-a-positive-control).
#
# A bd that returns an empty list "[]" for every query: zero ready beads, zero waiting
# asks. SP_READY and SP_NEXT_N should be 0, not ?.

BD_EMPTY="$TMP/bd-empty"
cat > "$BD_EMPTY" <<'EOF'
#!/usr/bin/env bash
printf '[]'
exit 0
EOF
chmod +x "$BD_EMPTY"

# ======================================================================================
# AN UNRESOLVABLE PERSONA IS A REFUSAL, NOT AN EMPTY QUEUE. The partition map is built
# from fayth_get, which resolves variables the way the summoner does. When SPIRA_FAYTHS
# names a persona that does not exist, fayth_get finds no file, the map stays empty, and
# core_detail_keys emits SP_READY=? rather than a zero (law-failed-probe-renders-question).

echo "partition map: an unresolvable persona is a REFUSAL, not an empty queue"
_unres_out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=no-such-persona \
    SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
    SPIRA_BD="$BD_EMPTY" \
    bash "$HERE/cockpit.sh" once 2>/dev/null)"
want   "SP_READY is ? when no persona resolves"   "SP_READY=?"  "$_unres_out"
nowant "SP_READY is NOT 0 when no persona resolves" "SP_READY=0" "$_unres_out"

echo "ready probe: bd returns empty list []:"
zero_out="$(run_probe "$BD_EMPTY")"
want   "SP_READY key is present"           "SP_READY="  "$zero_out"
nowant "SP_READY is NOT ? for empty list"  "SP_READY=?" "$zero_out"
want   "SP_NEXT_N key is present"          "SP_NEXT_N=" "$zero_out"
nowant "SP_NEXT_N is NOT ? for empty list" "SP_NEXT_N=?" "$zero_out"
want   "SP_WAITING key is present"         "SP_WAITING="  "$zero_out"
nowant "SP_WAITING is NOT ? for empty list" "SP_WAITING=?" "$zero_out"

# ======================================================================================
# THE FAILURE CASE. A bd that mimics a schema-mismatch refusal: prints the error to
# stdout and exits 0. json_only strips the non-JSON line, so bdjson produces no output.
# Before the fix, the ready and next-up probes counted nothing as "zero ready beads".

BD_REFUSED="$TMP/bd-refused"
cat > "$BD_REFUSED" <<'EOF'
#!/usr/bin/env bash
echo "schema version mismatch: database is at v61, binary knows up to v53"
exit 0
EOF
chmod +x "$BD_REFUSED"

echo ""
echo "ready probe: bd refuses (schema mismatch, exit 0):"
ref_out="$(run_probe "$BD_REFUSED")"
want "SP_READY is ? on refusal"   "SP_READY=?"   "$ref_out"
want "SP_NEXT_N is ? on refusal"  "SP_NEXT_N=?"  "$ref_out"
want "SP_WAITING is ? on refusal" "SP_WAITING=?" "$ref_out"
tl_summary
