#!/usr/bin/env bash
#
# test-czar-shadow.sh — czar shadow mode: per-class stage key and fence
#
# THE DEFECT THIS GUARDS. Without a shadow/act stage the czar is all-or-nothing:
# enabling a class requires adding it to SPIRA_FAYTHS, at which point it immediately
# acts on every trigger it sees. A shadow stage lets the operator watch what the czar
# WOULD do without it touching the queue (law-hand-over-in-four-stages, stage 2).
#
# WHAT THIS SUITE CHECKS.
#   1. czar-fence.sh exits 1 (shadow) when SPIRA_CZAR_STAGE_<CLASS> is unset — default.
#   2. czar-fence.sh exits 0 (act) when set to act.
#   3. queue.sh eject is refused when SPIRA_FAYTH=czar and SPIRA_CZAR_CLASS is set and the
#      class is in shadow; act mode and non-czar callers are unaffected.
#   4. All 7 class names map to the correct env var suffix (hyphens → underscores).
#   5. czar.fayth FAYTH_TOOLS includes czar-fence.sh.
#   6. czar.md brief mentions CZAR-WOULD note format and czar-fence.sh.
#   7. All 7 SPIRA_CZAR_STAGE_* keys are in the SPIRA_CONF_KEYS allowlist.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): the shadow refusal is
# asserted FIRST. Only when that failure is confirmed does the suite believe the
# act-mode acceptance. A fence that silently allows both modes is indistinguishable
# from a fence that is absent.
#
# The 11 static conf/fayth/brief grep rows this suite used to carry (czar.fayth names
# czar-fence.sh, czar.md names CZAR-WOULD/shadow/czar-fence.sh, and all 7 stage keys are in
# conf.sh's allowlist) moved to test-czar-lint.sh (T0) — they read text, not behaviour, and
# gap 11 of docs/test-plan/safety-fences.md is the citation for the move.
#
# tier: T1
# covers: spira/czar-fence.sh spira/queue.sh spira/lib.sh spira/aeon.sh spira/watchtower.sh UC-safety-fences-31
# hermetic-ok: no database, no systemd; queue.sh fence fires before any db access
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1: did not want [$2] in [$3]"; }

FENCE="$HERE/czar-fence.sh"
[ -x "$FENCE" ] || { printf 'czar-fence.sh not found or not executable: %s\n' "$FENCE" >&2; exit 2; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

echo "test-czar-shadow.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL — shadow (default) refuses; act allows"
# ==========================================================================================

# SEEN RED: czar-fence.sh deadlock exits 1 when stage is unset (default=shadow).
out="$(SPIRA_CZAR_STAGE_DEADLOCK="" "$FENCE" deadlock 2>&1 || true)"
rc=0; SPIRA_CZAR_STAGE_DEADLOCK="" "$FENCE" deadlock >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] && ok "czar-fence deadlock exits 1 in shadow (default)" \
                || bad "czar-fence deadlock: expected exit 1 in shadow, got $rc"
want "refusal message names the stage var" "SPIRA_CZAR_STAGE_DEADLOCK" "$out"
want "refusal message names the class" "deadlock" "$out"

# SEEN GREEN: czar-fence.sh deadlock exits 0 when stage is act.
rc=0; SPIRA_CZAR_STAGE_DEADLOCK=act "$FENCE" deadlock >/dev/null 2>&1 && rc=0 || rc=$?
[ "$rc" -eq 0 ] && ok "czar-fence deadlock exits 0 in act" \
                || bad "czar-fence deadlock: expected exit 0 in act, got $rc"

# ==========================================================================================
echo
echo "czar + shadow → queue.sh eject refused; act and non-czar unaffected"
# ==========================================================================================
# The fence is bound inside cmd_eject before any db access, so SPIRA_DB need not be real.
TQ="$(mktemp -d)"; mkdir -p "$TQ/run"

# SEEN RED: czar in shadow — queue.sh eject refused with czar-fence message.
out="$(SPIRA_FAYTH=czar SPIRA_CZAR_CLASS=deadlock \
       SPIRA_CONF=/nonexistent SPIRA_RUN="$TQ/run" SPIRA_DB="$TQ/nodb" \
       bash "$HERE/queue.sh" eject sp-fake 2>&1 || true)"
[[ "$out" == *"czar-fence"* && "$out" == *"shadow"* ]] \
    && ok "czar eject in shadow: fence fires inside queue.sh" \
    || bad "czar eject in shadow: expected czar-fence shadow message, got: $out"

# SEEN GREEN: czar in act — no shadow refusal (may fail for other reasons; that is expected).
out2="$(SPIRA_FAYTH=czar SPIRA_CZAR_CLASS=deadlock SPIRA_CZAR_STAGE_DEADLOCK=act \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TQ/run" SPIRA_DB="$TQ/nodb" \
        bash "$HERE/queue.sh" eject sp-fake 2>&1 || true)"
[[ "$out2" != *"czar-fence"*shadow* ]] \
    && ok "czar eject in act: no shadow refusal" \
    || bad "czar eject in act: unexpected shadow refusal: $out2"

# NON-CZAR: unaffected — no czar-fence message regardless of queue outcome.
out3="$(SPIRA_CONF=/nonexistent SPIRA_RUN="$TQ/run" SPIRA_DB="$TQ/nodb" \
        bash "$HERE/queue.sh" eject sp-fake 2>&1 || true)"
[[ "$out3" != *"czar-fence"* ]] \
    && ok "non-czar eject: fence not triggered" \
    || bad "non-czar eject: unexpected czar-fence message: $out3"

rm -rf "$TQ"

# ==========================================================================================
echo
echo "class name → env var mapping"
# ==========================================================================================
# Each class name (hyphens → underscores, uppercase) maps to its own stage key.
for pair in "attribution-failed:ATTRIBUTION_FAILED" \
            "sort-failed:SORT_FAILED" \
            "loop-stalled:LOOP_STALLED" \
            "ci-stalled:CI_STALLED" \
            "starved:STARVED" \
            "ci-red:CI_RED"; do
    cls="${pair%%:*}"; sfx="${pair##*:}"
    var="SPIRA_CZAR_STAGE_${sfx}"
    # Shadow by default:
    rc=0; eval "${var}='' \"$FENCE\" \"$cls\"" >/dev/null 2>&1 && rc=0 || rc=$?
    [ "$rc" -eq 1 ] && ok "$cls → $var (shadow default)" \
                    || bad "$cls → $var: expected exit 1 in shadow, got $rc"
    # Act when set:
    rc=0; eval "${var}=act \"$FENCE\" \"$cls\"" >/dev/null 2>&1 && rc=0 || rc=$?
    [ "$rc" -eq 0 ] && ok "$cls → $var (act mode)" \
                    || bad "$cls → $var: expected exit 0 in act, got $rc"
done

echo
tl_summary
