#!/usr/bin/env bash
#
# test-dwarf-elastic-concurrency.sh — dwarf.sh's own concurrency check honours RUNE_ELASTIC,
#   using the same rune_free (lib.sh) the sentinel already calls, so the two cannot drift.
#
#   ./test-dwarf-elastic-concurrency.sh
#
# THE DEFECT (sp-4gxjo). The sentinel calls an elastic persona up to the declared pool
# (SPIRA_MAX_DWARVES) via rune_free, which honours RUNE_ELASTIC. dwarf.sh's OWN concurrency
# check — run by the dwarf itself right after it wakes, before it claims anything — compared
# `have` against RUNE_MAX_CONCURRENT alone and never looked at RUNE_ELASTIC at all. With a
# pool of 6 and RUNE_MAX_CONCURRENT=3 (the fallback for a host with no pool), the sentinel
# kept calling past the third builder and every one of them exited immediately logging "at
# capacity (3/3)" — five wasted calls in eight minutes on 2026-09-24.
#
# THE FIX. dwarf.sh now computes the same pool-remainder the sentinel computes before calling
# rune_free (pool = SPIRA_MAX_DWARVES - have, only when RUNE_ELASTIC=1), then asks rune_free
# the same question CHECK 7 already asks. A non-elastic rune gets no pool argument, so
# rune_free falls through to RUNE_MAX_CONCURRENT exactly as the old inline check did.
#
# T1, over the shared function (rune_free itself is already covered by test-call-rune.sh):
#   1. elastic, pool=6, 5 live  -> allowed (dwarf proceeds past the capacity gate)
#   2. elastic, pool=6, 6 live  -> refused ("at capacity (6/6)")
#   3. non-elastic (no pool draw), MAX_CONCURRENT=2: 1 live -> allowed, 2 live -> refused
#      (regression check: the fallback path this fix must not disturb)
#
# "ALLOWED" IS OBSERVED WITHOUT A DATABASE. Rather than standing up testdb/bd to let the dwarf
# claim a real bead, a world.halted stamp is armed before each "allowed" run: dwarf.sh checks
# concurrency FIRST and halted SECOND, so a dwarf that clears the capacity gate reaches the
# halted gate immediately afterward and exits there with a distinct, unambiguous log line
# ("halted — claiming nothing") and a distinct ledger reason ("awake $RUNE halted"). A dwarf
# that is refused never reaches that line at all — it exits at the capacity gate with "awake
# $RUNE capacity". The two ledger reasons cannot be confused for one another.
#
# dwarf_count IS STUBBED (a fake lib.sh sourced in place of the real one, which itself sources
# the real lib.sh so rune_free is exercised unmodified) so the "have" side of the comparison
# is a controlled number rather than a real process count.
#
# defect: sp-4gxjo
# tier: T1
# covers: dwarf/src/* spira/lib.sh spira/runes/builder.rune
# hermetic-ok: no database, no systemd; dwarf_count is stubbed, SPIRA_AGENT is never reached
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-dwarf-elastic-concurrency.sh"

# dwarf and spira-claim are invoked by name, from the tree's build on the suite's PATH.
# The dwarf binary (dwarf/, which replaced dwarf.sh) must be built — a refusal to run at all
# here is louder than a suite that silently runs nothing.
command -v dwarf >/dev/null 2>&1 \
    || { echo "test-dwarf-elastic-concurrency.sh: dwarf is not on PATH — refusing to run" >&2; exit 1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Minimal harness tree: the dwarf binary runs with --home here, against a stub lib.sh that
# sources the real one; everything else is read from the real checkout.
#
# conf.d IS COPIED IN (matching test-dwarf-sweep.sh, test-dwarf-world-stop.sh, ...): since wave
# 4.8 (sp-mz7dn), dwarf's own capacity check reads SPIRA_MAX_DWARVES through the in-process
# spira_config::resolve() registry pass (dwarf::conf::merge_resolved_config), which derives
# conf.d from THIS --home, not — the way the bash seam's conf.sh derives it from its own
# BASH_SOURCE — from wherever lib.sh/conf.sh actually live. A --home with no conf.d resolves
# the whole generic registry to nothing, so SPIRA_MAX_DWARVES=6 (exported below) never reaches
# dwarf.v despite being set in the environment: dwarf fell back to RUNE_MAX_CONCURRENT alone
# ("at capacity (5/3)" instead of honouring the pool), exactly the defect sp-4gxjo fixed,
# reintroduced through the config seam instead of the inline check this time (sp-8qm8g).
export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/runes"
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
# SPIRA_RUNES_DIR is registered and the fixture declares a fixed, nonexistent path — nothing
# derives it from SPIRA_HOME any more (sfail round 2, pattern 6).
tl_config SPIRA_RUNES_DIR="$SPIRA_HOME/runes"
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
tl_config SPIRA_RUN="$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
tl_config SPIRA_DB="$TMP/no-db"
# The fence (rune_fenced, lib.sh) refuses an empty RUNE_LABELS and, when SPIRA_SCOPE_LABEL
# is set, requires the label naming this scope. Neither is what this suite is testing, so
# disable scope restriction the same way an operator would to allow an unrestricted predicate.
tl_config SPIRA_SCOPE_LABEL=""

# The stub: source the REAL lib.sh (so rune_free is the genuine article), then override
# dwarf_count so "have" is a controlled number rather than a real /proc scan.
cat > "$SPIRA_HOME/lib.sh" <<STUB
# shellcheck disable=SC1090
. "$HERE/lib.sh"
dwarf_count() { printf '%s' "\${MOCK_DWARF_COUNT:-0}"; }
STUB

run_dwarf() {   # run_dwarf <rune> -> stdout+stderr captured
    dwarf --home "$SPIRA_HOME" "$1" 2>&1
}
refused() { [[ "$1" == *"at capacity"* ]]; }

LEDGER="$SPIRA_RUN/dwarf-ledger.log"

# ======================================================================================
echo
echo "1 — elastic, pool declared (SPIRA_MAX_DWARVES=6): 5 live is allowed, 6 live is refused"
# ======================================================================================
cat > "$SPIRA_HOME/runes/builder.rune" <<'F'
RUNE_NAME=builder
RUNE_WORKING_EXITS="SUBMITTED ask park hold"
RUNE_LABELS="test,plan"
RUNE_MAX_CONCURRENT=3
RUNE_ELASTIC=1
F
tl_config SPIRA_MAX_DWARVES=6

: > "$LEDGER"
export MOCK_DWARF_COUNT=5
rm -f "$SPIRA_RUN/world.halted"; : > "$SPIRA_RUN/world.halted"
out="$(run_dwarf builder)"
if refused "$out"; then
    bad "pool=6, 5 live: allowed" "refused: $out"
else
    ok "pool=6, 5 live: not refused by the capacity gate"
fi
want "pool=6, 5 live: reached the halted gate (proves it cleared capacity)" \
     "halted — claiming nothing" "$out"
want "ledger records the halted exit, not a capacity exit" \
     "awake builder halted" "$(cat "$LEDGER")"
nowant "ledger does not record a capacity exit" "awake builder capacity" "$(cat "$LEDGER")"

: > "$LEDGER"
export MOCK_DWARF_COUNT=6
rm -f "$SPIRA_RUN/world.halted"
out="$(run_dwarf builder)"
want "pool=6, 6 live: refused, and names have/limit" "at capacity (6/6)" "$out"
want "ledger records the capacity exit" "awake builder capacity" "$(cat "$LEDGER")"
nowant "never reached the halted gate" "halted — claiming nothing" "$out"

# ======================================================================================
echo
echo "2 — positive control: with 2 free slots (pool=6, 4 live) elastic is still allowed"
# ======================================================================================
: > "$LEDGER"
export MOCK_DWARF_COUNT=4
: > "$SPIRA_RUN/world.halted"
out="$(run_dwarf builder)"
if refused "$out"; then
    bad "pool=6, 4 live: allowed" "refused: $out"
else
    ok "pool=6, 4 live: not refused (two slots free, not just the boundary)"
fi

# ======================================================================================
echo
echo "3 — non-elastic rune: unaffected by SPIRA_MAX_DWARVES, still uses its own cap"
# ======================================================================================
# Regression check: a rune that never declared RUNE_ELASTIC must keep the old behaviour
# exactly — refused at its own RUNE_MAX_CONCURRENT regardless of how large the pool is.
cat > "$SPIRA_HOME/runes/anchor.rune" <<'F'
RUNE_NAME=anchor
RUNE_LABELS="test,incident"
RUNE_MAX_CONCURRENT=2
F
# SPIRA_MAX_DWARVES stays at 6 (non-default, still set from section 1) — a non-elastic rune
# must ignore it entirely, so a large pool must not let it exceed its own smaller cap.

: > "$LEDGER"
export MOCK_DWARF_COUNT=1
: > "$SPIRA_RUN/world.halted"
out="$(run_dwarf anchor)"
if refused "$out"; then
    bad "non-elastic, cap=2, 1 live: allowed" "refused: $out"
else
    ok "non-elastic, cap=2, 1 live: not refused"
fi

: > "$LEDGER"
export MOCK_DWARF_COUNT=2
rm -f "$SPIRA_RUN/world.halted"
out="$(run_dwarf anchor)"
want "non-elastic, cap=2, 2 live: refused at its own cap, not the pool" \
     "at capacity (2/2)" "$out"

echo
tl_summary
