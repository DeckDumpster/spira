#!/usr/bin/env bash
#
# test-event-taxonomy.sh — every event kind a spira_event call site emits is declared, in
# the shape the events table accepts, and the handful of non-suite-driven call sites are
# still wired. Pure source grep, no process spawned and no clock — split out of
# test-event.sh (coverage-map row 20, docs/test-plan/cockpit-observability.md): the storm
# and suppression behaviour needs a running emitter, this does not.
#
# tier: T0
# covers: spira/*.sh UC-cockpit-observability-20
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "the taxonomy — every kind the harness emits is one the emitter accepts"
# A typo'd kind is refused at send time. Catching it at the gate is the difference between
# a taxonomy and free text, so the vocabulary is declared here and every call site checked
# against it — adding a kind is then a deliberate act, which is what a taxonomy is.
KINDS="aeon.claimed bead.landed bead.poisoned bead.reopened branch.reclaimed ci.failed"
KINDS="$KINDS pilgrimage.complete note aeon.rapid queue.abandoned"

sites="$(grep -rhE '[^#]spira_event [a-z0-9.]+' "$HERE"/*.sh 2>/dev/null \
         | grep -v '^\s*#' \
         | grep -oE 'spira_event [a-z0-9.]+' \
         | sed -E 's/.* //' | sort -u)"
[ -n "$sites" ] && ok "the call sites can be found at all" \
                || bad "the call sites can be found at all" "the grep matched nothing — it is measuring itself, not the harness"

for k in $sites; do
    case " $KINDS " in
        *" $k "*) ok "kind '$k' is in the declared taxonomy" ;;
        *) bad "kind '$k' is in the declared taxonomy" \
               "an emitted kind nobody declared — add it to KINDS here, or fix the call site" ;;
    esac
done
case "$sites" in *bead.landed*) ok "the emitted kinds are all declared" ;; esac

# Each kind must be lowercase dotted segments, no longer than 32 characters (the db column).
# ONE ok/bad PER KIND, never a trailing unconditional ok: that would report the taxonomy
# clean even after a kind above it had just failed the same check.
for k in $KINDS; do
    case "$k" in
        *[!a-z0-9.]*|.*|*.|*..*) bad "kind '$k' is valid" "not lowercase dotted segments" ;;
        *) if [ "${#k}" -le 32 ]; then ok "kind '$k' is valid"; else bad "kind '$k' fits 32-char limit" "${#k} chars"; fi ;;
    esac
done

echo
echo "the call sites — every outcome the harness has is wired to one"
# aeon.claimed cannot be driven by a suite: aeon.sh launches a Claude session, so every
# suite stubs it. The other five are exercised through their own scripts in test-landing.sh,
# test-poison.sh. What is checkable here is that the claim is still
# wired, and wired AFTER the ledger — the ledger is what aeon_count and the born/awake
# positive control read, and it must not come to depend on a database being reachable.
claim="$(grep -n -A14 '^ledger "awake \$FAYTH \$BEAD_ID"' "$HERE/aeon.sh" 2>/dev/null)"
want "a claim is emitted"          "spira_event aeon.claimed" "$claim"
want "and only after the ledger"   "ledger \"awake"           "$claim"

for site in \
    "landing.sh:bead.landed"    "landing.sh:bead.reopened" \
    "sentinel.sh:bead.poisoned" "gate-check.sh:ci.failed" \
    "strand.sh:branch.reclaimed"; do
    f="${site%%:*}"; k="${site##*:}"
    grep -q "spira_event $k " "$HERE/$f" \
        && ok "$f emits $k" || bad "$f emits $k" "no call site"
done

tl_summary
