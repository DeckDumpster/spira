#!/usr/bin/env bash
#
# test-cadence-seams.sh — cadence.sh's pure seams: span→OnCalendar mapping, the drop-in
# text, and its refusals — split off T1 from test-cadence-tool.sh's real-systemd suite
# (T4-ish, container-only) so a defect in these does not wait on podman to be seen.
#
# tier: T1
# covers: spira/cadence.sh UC-instance-lifecycle-32
#
# WHAT STAYS AT T4. Whether systemd actually reports a next elapse for a given [Timer]
# stanza is a real-systemd fact, not something these functions can answer on their own; the
# "naive override disarms, cadence.sh does not" case (sp-dah) stays in test-cadence-tool.sh.
# What IS provable here without systemd: what text render_schedule and _dropin_for produce,
# and what resolve_unit/cmd_verify refuse — none of the three touches $SYSTEMCTL when its
# inputs are what these cases give it (an empty or a small fixture directory).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

export SPIRA_SYSTEMD_USER_DIR="$SCRATCH"
export SPIRA_INSTANCE=prod
export SPIRA_OPERATOR=testop
export SPIRA_TZ=UTC
# shellcheck disable=SC1091
. "$HERE/cadence.sh"

echo "test-cadence-seams.sh"

# ============================================================================
echo
echo "resolve_unit — no timers installed:"
# ============================================================================
# POSITIVE CONTROL for the fixture dir itself: an empty $SPIRA_SYSTEMD_USER_DIR must be
# reported as such before any resolution verdict below is trusted.
_out="$(resolve_unit anything 2>&1)"
want "empty fixture dir is refused by name" "no timer units installed under $SCRATCH" "$_out"

# ============================================================================
echo
echo "resolve_unit — naming (exact, suffixed, prefixed, ambiguous, unknown):"
# ============================================================================
: > "$SCRATCH/spira-groom-prod.timer"
: > "$SCRATCH/spira-verdict-prod.timer"
: > "$SCRATCH/cockpit-ensure.timer"

is "bare name + spira- prefix + instance suffix resolves" \
    "spira-groom-prod.timer" "$(resolve_unit groom)"
is "shared unit's exact filename resolves" \
    "cockpit-ensure.timer" "$(resolve_unit cockpit-ensure)"

_amb="$(resolve_unit spira 2>&1)"
want "an ambiguous prefix is refused, not guessed" "names more than one installed timer" "$_amb"

_unk="$(resolve_unit definitely-not-a-real-unit 2>&1)"
want "an unknown unit names the fact, not a guess" "no installed timer matches" "$_unk"

# ============================================================================
echo
echo "render_schedule — span becomes wall clock wherever one exists exactly:"
# ============================================================================
# POSITIVE CONTROL: an already-OnCalendar expression passes through untouched.
is "an OnCalendar expression passes through untouched" \
    "OnUnitActiveSec=
OnBootSec=
OnCalendar=Mon *-*-* 09:00:00" \
    "$(render_schedule 'Mon *-*-* 09:00:00')"

is "an hour count that divides 24 maps to OnCalendar" \
    "OnUnitActiveSec=
OnBootSec=
OnCalendar=*-*-* 0/6:00:00" \
    "$(render_schedule 6h)"

is "24h maps to the midnight form, not 0/24" \
    "OnUnitActiveSec=
OnBootSec=
OnCalendar=*-*-* 00:00:00" \
    "$(render_schedule 24h)"

is "a minute count that divides 60 maps to OnCalendar" \
    "OnUnitActiveSec=
OnBootSec=
OnCalendar=*-*-* *:0/15:00" \
    "$(render_schedule 15min)"

# NEGATIVE CONTROL / backstop: an hour count with no clean calendar equivalent (24 % 7 != 0)
# keeps the span, but is never left as the ONLY trigger.
_bs="$(render_schedule 7h)"
want "a span with no calendar equivalent keeps the span" "OnUnitActiveSec=7h" "$_bs"
want "and is paired with a daily calendar backstop" "OnCalendar=*-*-* 00:00:00" "$_bs"

# ============================================================================
echo
echo "_dropin_for — the drop-in text carries cadence, reason and revert line:"
# ============================================================================
_d="$(_dropin_for spira-verdict-prod.timer 6h 'suite reason')"
want "it names the requested cadence" "Requested cadence: 6h" "$_d"
want "it records why" "Why: suite reason" "$_d"
want "it shows the shipped template's own cadence" "60 2min" "$_d"
want "it names the exact revert command" "cadence.sh clear spira-verdict-prod.timer" "$_d"
want "it embeds render_schedule's own [Timer] lines" "OnCalendar=*-*-* 0/6:00:00" "$_d"

_d_nowhy="$(_dropin_for spira-verdict-prod.timer 6h '')"
nowant "an empty --why omits the Why: line" "Why:" "$_d_nowhy"

# ============================================================================
echo
echo "cmd_verify — an empty sweep refuses to report all-clear:"
# ============================================================================
# Zero installed timers means the check found nothing to check; reporting "all armed" on
# nothing displaces the suspicion that would have prompted a look
# (law-absence-needs-a-positive-control). Emptying the fixture dir again exercises this
# without ever reaching $SYSTEMCTL — cmd_verify's loop body never runs on an empty list.
rm -f "$SCRATCH"/*.timer
_vout="$(cmd_verify 2>&1)"; _vrc=$?
want "it says it found nothing to check" "refusing to report all-clear" "$_vout"
wantrc "and exits non-zero" 1 "$_vrc"

tl_summary
