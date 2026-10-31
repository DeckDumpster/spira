#!/usr/bin/env bash
#
# test-epic-claim-rank-failure.sh — T8 (sp-o4trx): a forced epic_rank_rows failure surfaces
# as a claim-error, never as idle. Split off test-epic-claim-order.sh for wall time; the
# shared fixture is testlib/epic-claim-fixture.sh.
#
# defect: sp-ns46j sp-o4trx
# tier: T3
# covers: spira/lib.sh dwarf/src/* spira-claim/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

ECL_SUITE=test-epic-claim-rank-failure
# shellcheck disable=SC1090
. "$HERE/testlib/epic-claim-fixture.sh"

echo "test-epic-claim-rank-failure.sh"

# ==========================================================================================
echo
echo "T8: a forced epic_rank_rows failure surfaces as a claim-error, never as idle (sp-o4trx)"
# ==========================================================================================
# THE SECOND HALF OF THE OUTAGE. Fixing the argv size alone is not enough: any OTHER reason
# epic_rank_rows might fail (a python crash, a corrupted temp file) must not read as "nothing
# ready to claim" either — the ledger line and the exit code must say claim-error.
seed <<JSONL
$(epic sp-e1 0)
$(closed_child sp-e1-done sp-e1)
$(bead sp-e1-rework sp-e1 3)
$(bead sp-unrelated-p0 "" 0)
JSONL
ecl_dwarf_env

# A spira-claim SHIM that fails only the final rank — the `select --resumable` call, the
# dwarf binary's epic_rank_rows (dwarf/src/claim.rs Selector::select) — and hands every other
# call (the epic lookup, the --top-tier band) to the real spira-claim, so the lookup and the
# resumability pass still run for real and only the rank itself is forced to fail. Ranking
# moved out of python (dwarf.sh's epic_rank_rows) into spira-claim with the Rust cutover, so
# the old python3 shim no longer reached the call under test.
REAL_CLAIM="$(command -v spira-claim)" \
    || { echo "test-epic-claim-rank-failure: spira-claim is not on PATH — T8 cannot force the rank" >&2; exit 1; }
BIN2="$TMP/bin2"; mkdir -p "$BIN2"
cat > "$BIN2/spira-claim" <<STUB
#!/usr/bin/env bash
for a in "\$@"; do
    if [ "\$a" = "--resumable" ]; then
        cat >/dev/null
        echo "T8 shim: forced epic_rank_rows failure" >&2
        exit 1
    fi
done
exec "$REAL_CLAIM" "\$@"
STUB
chmod +x "$BIN2/spira-claim"

DWARF_RUN2="$TMP/dwarfrun2"; mkdir -p "$DWARF_RUN2"
# SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
# CONFIG): declare via tl_config, not the env prefix below, which no process reads any more.
tl_config SPIRA_RUN="$DWARF_RUN2" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$DWARF_REPO_MAP"
( PATH="$BIN2:$TMP/lc-dwarf:$PATH" SPIRA_HOME="$DWARF_HOME" \
  SPIRA_CONF="$TMP/no-such3.conf" \
  dwarf --home "$DWARF_HOME" builder > "$TMP/dwarf-out2" 2>&1 )
t8_rc=$?

[ "$t8_rc" -ne 0 ] && ok "dwarf.sh exits non-zero on a forced rank failure (never the 'idle' success exit)" \
                   || bad "dwarf.sh exits non-zero on a forced rank failure (never the 'idle' success exit)" "rc=0"
want "the log names it a claim-error, not idle" "claim-error" "$(cat "$TMP/dwarf-out2")"
t8_ledger="$(cat "$DWARF_RUN2/dwarf-ledger.log" 2>/dev/null)"
want "the ledger records claim-error" "claim-error" "$t8_ledger"
is "the ledger never records this as an idle calls" "0" \
    "$(printf '%s\n' "$t8_ledger" | grep -c ' idle$')"

tl_summary
