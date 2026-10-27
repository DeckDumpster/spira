#!/usr/bin/env bash
#
# test-summon-rune.sh — what only the real bash and the real binaries can prove: lib.sh's
# rune_free arithmetic, `aeon --escape` through the world gate, and world.sh drain's
# expiry writer. summon_rune's refusal ladder (fleet ceiling, elastic and lane last-slot
# reservations, pool, halt, drain TTL, no CPUQuota/Nice) is sentinel's own Rust tests now.
#
#   ./test-summon-rune.sh
#
# POSITIVE CONTROLS BEFORE EACH REFUSAL (law-absence-needs-a-positive-control).
#
# tier: T1
# covers: spira/lib.sh sentinel/src/summon.rs aeon/src/escape.rs spira-world/src/bin/world.rs UC-dispatch-09 UC-dispatch-10 UC-dispatch-11 UC-dispatch-12 UC-dispatch-15 UC-dispatch-23
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run" "$T/runes" "$T/bin"
# SPIRA_RUNES_DIR no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at this suite's own fixture runes explicitly.
tl_config SPIRA_RUNES_DIR="$T/runes"

# `aeon --escape <rune>` (spira/escape.sh retired, sp-zpaq0) resolves its home from
# SPIRA_HOME and requires <home>/lib.sh to exist; this one-line stub sources the REAL
# lib.sh from $HERE.
printf '. "%s/lib.sh"\n' "$HERE" > "$T/lib.sh"

# conf.d IS COPIED IN (matching test-aeon-sweep.sh, ...):
# aeon derives its config registry from --home/SPIRA_HOME and now REFUSES to start if
# conf.d is missing (sp-1cdgq) -- a --home with no conf.d used to resolve silently to
# nothing instead of refusing. Without this, `aeon --escape` below dies at config
# resolution before it ever reaches escape.rs's own logic.
cp -r "$HERE/conf.d" "$T/"

# A MINIMAL ENVIRONMENT, non-default everywhere so that assertions cannot pass by reading
# literals out of conf.sh (law-gates-run-in-a-clean-environment). $T/bin goes FIRST so our
# own spira-claim/mock-summon stubs shadow the release's real ones; the release's real
# `aeon` (never placed in $T/bin) still resolves further down the same PATH.
SPIRA_RUN="$T/run"; tl_config SPIRA_RUN="$SPIRA_RUN"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T" PATH="$T/bin:$T:$PATH"
SPIRA_DB="$T/no-db"; tl_config SPIRA_DB="$SPIRA_DB"

# THE AEON IS A BINARY (aeon.sh is gone): summon_rune hands systemd-run the aeon it finds
# on PATH (sp-gypjk). The mock SPIRA_SUMMON never execs it.
command -v aeon >/dev/null 2>&1 \
    || { echo "test-summon-rune: aeon is not on PATH" >&2; exit 1; }

. "$HERE/lib.sh"

# ======================================================================================
# THE SHARED STUB SET. No real database, no real systemd, no real process table.
# ======================================================================================

# rune_free (lib.sh) is UNCHANGED by wave 4.27 — it stays real bash, calling aeon_count
# by name in THIS shell, so a bash-function stub still reaches it directly.
MOCK_COUNT=0
aeon_count() { printf '%d' "$MOCK_COUNT"; }

# set_ready <rune> <n> -> rune_ready(<rune>) as `summon_rune` (now a `sentinel` shim,
# wave 4.27) reaches it: `spira-claim rune-ready`, a real subprocess on PATH, never a
# bash function the fresh sentinel process could see.
set_ready() { printf '%s' "${2:-0}" > "$T/ready-$1"; }
cat > "$T/bin/spira-claim" <<SPIRACLAIM
#!/usr/bin/env bash
if [ "\${1:-}" = rune-ready ]; then
    cat "$T/ready-\${2:-}" 2>/dev/null || printf 0
    exit 0
fi
exit 0
SPIRACLAIM
chmod +x "$T/bin/spira-claim"

SUMMONED="$T/summoned.log"
cat > "$T/bin/mock-summon" <<'MOCK'
#!/usr/bin/env bash
rune="${@: -1}"
[ "$rune" = "--dry-run" ] && rune="${@: -2:1}"
printf 'SUMMONED:%s\n' "$rune" >> "$SUMMONED_FILE"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"
export SPIRA_SUMMON="$T/bin/mock-summon"
export SUMMONED_FILE="$SUMMONED"

# ======================================================================================
# THREE SYNTHETIC RUNES, none named "builder" or "ops" — every rule below must bind to a
# declaration (RUNE_ELASTIC, RUNE_LANE), never to a persona's name.
# ======================================================================================
cat > "$T/runes/stretchy.rune" <<'F'
RUNE_NAME=stretchy
RUNE_LABELS="test,plan"
RUNE_EXCLUDE_LABELS="test-poison"
RUNE_MAX_CONCURRENT=4
RUNE_ELASTIC=1
RUNE_HEARTBEAT_SECONDS=120
F

cat > "$T/runes/anchor.rune" <<'F'
RUNE_NAME=anchor
RUNE_LABELS="test,incident"
RUNE_EXCLUDE_LABELS="test-poison"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=60
F

cat > "$T/runes/laner.rune" <<'F'
RUNE_NAME=laner
RUNE_LABELS="test,ops"
RUNE_MAX_CONCURRENT=1
RUNE_HEARTBEAT_SECONDS=60
RUNE_LANE=ops
F

DRAIN_STAMP="$T/run/world.draining"
HALT_STAMP="$T/run/world.halted"
echo
echo "drain stamp — world.sh drain gates until resume; --for caps the wait, never the gate"
rm -f "$DRAIN_STAMP"
SPIRA_RUN="$SPIRA_RUN" world drain --for 900 --timeout 1 >/dev/null 2>&1
if [ -f "$DRAIN_STAMP" ]; then
    if grep -qx 'holds until resume' "$DRAIN_STAMP"; then
        ok "world.sh drain stamp holds until resume"
    else
        bad "world.sh drain stamp holds until resume" "no 'holds until resume' line in the stamp"
    fi
    if grep -q '^expires ' "$DRAIN_STAMP"; then
        bad "world.sh drain stamp has no expiry" "an expires line would let a clock lift the gate"
    else
        ok "world.sh drain stamp has no expiry"
    fi
else
    bad "world.sh drain writes a stamp" "no stamp at $DRAIN_STAMP"
fi
rm -f "$DRAIN_STAMP"

# ======================================================================================
echo
echo "rune_free — an elastic persona takes the pool remainder whole, never subtracting running twice"
# ======================================================================================
for n in 0 1 2 3 4 5; do
    pool=$((5 - n))
    MOCK_COUNT=$n
    got="$(rune_free stretchy "$pool")"
    is "elastic n=$n, pool=$pool -> free=$pool" "$pool" "$got"
done

# THE ROW THAT REPLACES rune-free's OWN VACUOUS "buggy formula" ARITHMETIC-ONLY CONTROL
# (G18): `have` and `pool` are set INDEPENDENTLY here, rather than tied together as
# pool=BASE-have above, so the old bug — subtracting the running count a second time from
# a number that already nets it out — is visible in one direct call rather than only by
# pattern across the loop.
MOCK_COUNT=3
got="$(rune_free stretchy 5)"
is "elastic: have=3 does not reduce a pool=5 remainder (max-have-on-a-remainder bug)" "5" "$got"

echo
echo "rune_free — no pool means fall back to the persona's own cap"
cap="$(rune_get stretchy RUNE_MAX_CONCURRENT 1)"
for n in 0 1; do
    MOCK_COUNT=$n
    got="$(rune_free stretchy)"
    want_val=$((cap - n))
    is "no pool, n=$n -> free=$want_val (cap=$cap)" "$want_val" "$got"
done

echo
echo "rune_free — a non-elastic persona: the running count is subtracted from its own cap"
anchor_cap="$(rune_get anchor RUNE_MAX_CONCURRENT 1)"
for n in 0 1; do
    MOCK_COUNT=$n
    got="$(rune_free anchor "10")"
    want_val=$((anchor_cap - n))
    is "anchor, pool=10, n=$n -> free=$want_val" "$want_val" "$got"
done

echo
echo "rune_free — a non-elastic lane rune without pool: cap minus running, clamped at 0"
MOCK_COUNT=0
is "laner: rune_free without pool returns cap-running" "1" "$(rune_free laner)"
MOCK_COUNT=1
is "laner at cap returns 0 free" "0" "$(rune_free laner)"
MOCK_COUNT=0

# ======================================================================================
echo
echo "aeon --escape — reaches a bead when the pool argument would have refused it"
# ======================================================================================
# SUBPROCESS STUB. `aeon --escape`'s own lib.sh seam sources lib.sh fresh in a new bash
# process (aeon/src/seam.rs), exactly as escape.sh did and exactly as `summon_rune`'s own
# `sentinel` shim now does for THIS script's direct calls too — neither sees a function
# this shell defines. SPIRA_BD is exported to a shim that answers "ready" queries from a
# sentinel file, so the subprocess's readiness can be toggled without a real Dolt
# database. Its real capacity_paused runs unstubbed; with no pause stamp at $SPIRA_RUN it
# answers "not paused" on its own.
export FAKE_READY_FILE="$T/run/fake-ready"
cat > "$T/bin/fake-bd" <<'FAKEBD'
#!/usr/bin/env bash
for arg; do
    if [ "$arg" = "ready" ]; then
        [ -f "${FAKE_READY_FILE:-}" ] && printf '[{"id":"sp-test"}]\n' || printf '[]\n'
        exit 0
    fi
done
exit 0
FAKEBD
chmod +x "$T/bin/fake-bd"
tl_config SPIRA_BD="$T/bin/fake-bd"

# `summon_rune`'s OWN readiness, below, goes through the spira-claim stub (set_ready) —
# $T/bin is ahead of the release's real spira-claim on PATH for every subprocess this
# whole suite spawns, `aeon --escape` included, so fake-bd's "ready" answer (read through
# a REAL spira-claim this stub shadows) is never reached either way; kept in sync here so
# both readinesses agree.
touch "$T/run/fake-ready"
set_ready stretchy 1

# escape.sh bypasses the pool and still summons.
rm -f "$SUMMONED"
aeon --escape stretchy 2>/dev/null || true
want "escape.sh summons despite pool=0 not being passed" "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

# THE CONTROL THAT MAKES THE ABOVE MEANINGFUL: escape.sh with nothing ready exits 0 but
# summons nothing — the summon above is about the rune having work, not the script
# always calling the binary unconditionally.
rm -f "$T/run/fake-ready" "$SUMMONED"
set_ready stretchy 0
aeon --escape stretchy 2>/dev/null || true
is "escape.sh with nothing ready does not summon" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
touch "$T/run/fake-ready"
set_ready stretchy 1

# ======================================================================================
echo
echo "G8 — escape.sh honours world_gate: a halt or live drain refuses the escape summon too (sp-uyw4n, sp-2w2wu)"
# ======================================================================================
touch "$T/run/fake-ready"

# POSITIVE CONTROL: with no halt or drain stamp, escape.sh still summons.
rm -f "$HALT_STAMP" "$T/run/world.draining" "$SUMMONED"
aeon --escape stretchy 2>/dev/null || true
is "G8 positive control: no halt/drain, escape.sh summons" \
   "SUMMONED:stretchy" "$(cat "$SUMMONED" 2>/dev/null)"

: > "$HALT_STAMP"
rm -f "$SUMMONED"
out="$(aeon --escape stretchy 2>&1)"; rc=$?
is "G8: halted — escape.sh does not summon" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
want "G8: escape.sh logs the halt refusal" "halted — not summoning" "$out"
is "G8: escape.sh exits non-zero when halted" "1" "$rc"
rm -f "$HALT_STAMP"

DRAIN_STAMP_G8="$T/run/world.draining"
{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) + 3600 ))"; } > "$DRAIN_STAMP_G8"
rm -f "$SUMMONED"
out="$(aeon --escape stretchy 2>&1)"; rc=$?
is "G8: live drain — escape.sh does not summon" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
want "G8: escape.sh logs the drain refusal" "draining — not summoning" "$out"
is "G8: escape.sh exits non-zero when draining" "1" "$rc"

{ echo "now"; echo "gated"; printf 'expires %s\n' "$(( $(date +%s) - 60 ))"; } > "$DRAIN_STAMP_G8"
rm -f "$SUMMONED"
aeon --escape stretchy 2>/dev/null || true
is "G8: a legacy past-expiry drain stamp still holds — escape.sh does not summon" "absent" "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
[ -f "$DRAIN_STAMP_G8" ] && ok "G8: the stamp is untouched" || bad "G8: the stamp is untouched" "a clock removed it"
rm -f "$DRAIN_STAMP_G8"

# ======================================================================================
echo
echo "escape.sh passes no CPUQuota and no Nice (sp-b4oct; it shares summon_argv since sp-9ce60.4)"
# ======================================================================================
# escape.sh shares summon_argv() with summon_rune, the same builder UC-dispatch-23 exercises
# directly below, so the absence is verified here at the escape.sh call site and again there.
QUOTA_ARGV="$T/escape-quota-argv"
cat > "$T/bin/mock-summon" <<MOCK
#!/usr/bin/env bash
printf '%s\n' "\$@" > "$QUOTA_ARGV"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"

rm -f "$QUOTA_ARGV"
export SPIRA_AEON_CPU_QUOTA=55
aeon --escape stretchy 2>/dev/null || true
want   "escape.sh: the summon argv was captured" "TimeoutStartSec=" "$(cat "$QUOTA_ARGV" 2>/dev/null)"
nowant "escape.sh: no CPUQuota, even with the retired knob set" "CPUQuota" "$(cat "$QUOTA_ARGV" 2>/dev/null)"
nowant "escape.sh: no Nice"                                    "Nice="    "$(cat "$QUOTA_ARGV" 2>/dev/null)"
unset SPIRA_AEON_CPU_QUOTA

cat > "$T/bin/mock-summon" <<'MOCK'
#!/usr/bin/env bash
rune="${@: -1}"
[ "$rune" = "--dry-run" ] && rune="${@: -2:1}"
printf 'SUMMONED:%s\n' "$rune" >> "$SUMMONED_FILE"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"

tl_summary
