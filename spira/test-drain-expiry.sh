#!/usr/bin/env bash
#
# test-drain-expiry.sh — a drain nobody resumes expires, and the summon gate lifts it loudly.
#
# WHY THIS SUITE EXISTS. A drain gates every summon and has no owner. Whoever sets one can
# die, exit, or simply forget before lifting it, and then the whole pipeline is stopped by a
# file. It has now happened three times:
#
#   2026-09-08  the session drained for a safe install and left it armed ~13 minutes. The
#               response was cockpit/health.sh's drain banner (test-drain-banner.sh).
#   2026-09-09  an Ops sweep applying sop-harness-checkout-behind drained at 18:02:03,
#               finished its SOP at 18:04:29, and exited without resuming. The world stayed
#               gated 59 MINUTES with 25 beads ready and zero aeons, while every sentinel
#               pass logged "pass complete — goal reached".
#
# A BANNER NEEDS SOMEBODY LOOKING; AN EXPIRY DOES NOT. That is why this exists on top of
# test-drain-banner.sh rather than instead of it: the banner makes a drain visible, and this
# makes a forgotten one harmless. The visible-drain beads (sp-r6wf, sp-v7ok) were themselves
# sitting unworked behind the drain — the failure seals itself, so display alone cannot be
# the whole answer.
#
# WHAT IS ASSERTED
#   1. POSITIVE CONTROL — a LIVE drain still gates. Without this, an expiry that lifted
#      everything unconditionally would pass every other case here.
#   2. An EXPIRED drain does not gate, and the stamp is removed.
#   3. Lifting is LOUD — the log says the drain expired and nobody resumed. A silent lift
#      would hide the forgotten resume, which is the defect worth seeing.
#   4. A stamp with NO `expires` line (written by the older world.sh) expires at
#      mtime + SPIRA_DRAIN_TTL, so an old stamp cannot wedge the loop forever.
#   5. world.sh drain WRITES the expiry, and --for sets it. This is the seam: if the writer
#      stops emitting the line the reader falls back to case 4, so both halves are pinned.
#
# defect: sp-9zs0y
# covers: spira/lib.sh spira/world.sh
# hermetic-ok: a stamp file and a stubbed summon, no database, no systemd
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
trap 'rm -rf "$T"; exit 130' INT TERM
mkdir -p "$T/run" "$T/chamber" "$T/bin"

export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T"
export SPIRA_DB="$T/no-db"
export SPIRA_DRAIN_TTL=1800

# shellcheck disable=SC1090
. "$HERE/lib.sh"

MOCK_READY=1
fayth_ready() { printf '%d' "$MOCK_READY"; }
aeon_count()  { printf '0'; }
capacity_paused() { return 1; }

cat > "$T/chamber/worker.fayth" <<'F'
FAYTH_NAME=worker
FAYTH_LABELS="spira,plan"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=2
FAYTH_ELASTIC=1
FAYTH_HEARTBEAT_SECONDS=120
F

SUMMONED="$T/summoned"
export SUMMONED_FILE="$SUMMONED"
export SPIRA_SUMMON="$T/bin/mock-summon"
cat > "$T/bin/mock-summon" <<'MOCK'
#!/usr/bin/env bash
fayth="${@: -1}"
[ "$fayth" = "--dry-run" ] && fayth="${@: -2:1}"
printf 'SUMMONED:%s\n' "$fayth" >> "$SUMMONED_FILE"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"

STAMP="$SPIRA_RUN/world.draining"
try() { rm -f "$SUMMONED"; summon_fayth worker 1 2>&1; }

echo "test-drain-expiry.sh"

# ======================================================================================
echo
echo "positive control — with no drain at all, the fayth IS summoned:"
# ======================================================================================
rm -f "$STAMP"
out="$(try)"
is "no drain: worker is summoned" "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"

# ======================================================================================
echo
echo "a LIVE drain still gates — the expiry must not lift what is genuinely held:"
# ======================================================================================
{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) + 3600 ))"; } > "$STAMP"
out="$(try)"
is "live drain: nothing summoned" "" "$(cat "$SUMMONED" 2>/dev/null)"
want "and it says it is draining" "draining — not summoning" "$out"
[ -f "$STAMP" ] && ok "the stamp survives a live drain" || bad "the stamp survives a live drain" "it was removed"

# ======================================================================================
echo
echo "an EXPIRED drain is lifted, loudly, and the stamp is removed:"
# ======================================================================================
{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) - 60 ))"; } > "$STAMP"
out="$(try)"
is "expired drain: worker IS summoned" "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"
want "the lift is loud"              "DRAIN EXPIRED" "$out"
want "and names the missing resume"  "did not resume" "$out"
[ -f "$STAMP" ] && bad "the stamp is removed" "it is still there" || ok "the stamp is removed"

# ======================================================================================
echo
echo "a stamp with NO expires line falls back to mtime + TTL:"
# ======================================================================================
# The compatibility case: a drain written by the older world.sh must not wedge the loop
# forever. Fresh mtime -> still gating; old mtime -> lifted.
{ echo "now"; echo "gated"; } > "$STAMP"
out="$(try)"
is "no expires line, fresh mtime: still gated" "" "$(cat "$SUMMONED" 2>/dev/null)"

{ echo "now"; echo "gated"; } > "$STAMP"
touch -d "@$(( $(date +%s) - SPIRA_DRAIN_TTL - 60 ))" "$STAMP"
out="$(try)"
is "no expires line, mtime past TTL: lifted" "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"
want "and it is loud about that too" "DRAIN EXPIRED" "$out"

# ======================================================================================
echo
echo "the seam — world.sh drain WRITES the expiry, and --for sets it:"
# ======================================================================================
# If the writer stops emitting this line the reader silently falls back to mtime+TTL, so the
# two halves are pinned together here rather than trusted to stay in step.
rm -f "$STAMP"
SPIRA_RUN="$SPIRA_RUN" bash "$HERE/world.sh" drain --for 900 --timeout 1 >/dev/null 2>&1
if [ -f "$STAMP" ]; then
    exp="$(sed -n 's/^expires \([0-9][0-9]*\)$/\1/p' "$STAMP" | head -1)"
    if [ -n "$exp" ]; then
        ok "world.sh drain writes an expires line"
        d=$(( exp - $(date +%s) ))
        if [ "$d" -gt 840 ] && [ "$d" -le 900 ]; then
            ok "--for 900 sets the deadline (${d}s out)"
        else
            bad "--for 900 sets the deadline" "expected ~900s out, got ${d}s"
        fi
    else
        bad "world.sh drain writes an expires line" "no expires line in the stamp"
    fi
else
    bad "world.sh drain writes a stamp" "no stamp at $STAMP"
fi
rm -f "$STAMP"

echo
printf 'test-drain-expiry.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
