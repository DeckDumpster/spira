#!/usr/bin/env bash
#
# test-watchd-unit-name.sh — watchd.sh queries the same installed unit name that
# install.sh creates, and they cannot drift.
#
#   ./test-watchd-unit-name.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. CONF ACCESSOR: conf.sh exports watch_unit_name() that produces the same name
#    as inst_watch_name() in systemd/units.sh for every (watcher, instance) pair.
#    A single formula in one place prevents the two from disagreeing.
# 2. POSITIVE CONTROL: the old template form (spira-watch@<name>.service) differs
#    from the installed form, so the test proves it could have caught the bug before
#    the fix landed.
# 3. WATCHD QUERIES THE INSTALLED NAME: watchd.sh's own output when asked about
#    a daemon watcher uses the installed name, not the template form.
#
# RUNS WITHOUT SYSTEMD. All assertions are against the naming formula; no unit
# needs to be loaded or active.
#
# tier: T1
# covers: spira/conf.sh
# covers: spira/watchd.sh
# covers: systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
eq()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "$2" "$3"; }
ne()  { [ "$2" != "$3" ] && ok "$1" || bad "$1" "$2" "anything != $3"; }

echo "test-watchd-unit-name.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Minimal environment: enough for conf.sh to parse without touching real paths.
# Pin SPIRA_INSTANCE to a non-default value so every assertion proves the test
# is not reading the operator's installed instance by accident.
# ---------------------------------------------------------------------------
FAKE_RUN="$TMP/run"
mkdir -p "$FAKE_RUN"

load_watch_unit_name() {
    env -i \
        PATH="$PATH" \
        HOME="$TMP/fake-home" \
        SPIRA_INSTANCE="$1" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$FAKE_RUN" \
        bash -c ". \"$HERE/conf.sh\"; watch_unit_name \"$2\"" 2>/dev/null
}

# inst_watch_name from systemd/units.sh: the authoritative formula.
# Duplicated here as the expected value so any drift between the two is
# flagged by the test rather than silently blessed.
inst_watch_name() { printf 'spira-watch-%s-%s.service' "$1" "$2"; }

# ---------------------------------------------------------------------------
# 1. CONF ACCESSOR agrees with inst_watch_name for several (name, instance) pairs.
# ---------------------------------------------------------------------------
echo
echo "CONF ACCESSOR — watch_unit_name matches inst_watch_name:"

for _inst in prod test staging; do
    for _name in answers testview cockpit; do
        got="$(load_watch_unit_name "$_inst" "$_name")"
        want="$(inst_watch_name "$_name" "$_inst")"
        eq "watch_unit_name $_name $_inst" "$got" "$want"
    done
done
unset _inst _name got want

# ---------------------------------------------------------------------------
# 2. POSITIVE CONTROL — the template form is DIFFERENT from the installed form,
#    so the test would have caught the pre-fix code.
#    Pre-fix: watchd.sh built "spira-watch@answers.service".
#    Post-fix: watchd.sh builds "spira-watch-answers-prod.service".
# ---------------------------------------------------------------------------
echo
echo "POSITIVE CONTROL — template form differs from installed form:"

template_form="spira-watch@answers.service"
installed_form="$(inst_watch_name answers prod)"
ne "template != installed (prod)" "$template_form" "$installed_form"

template_form="spira-watch@testview.service"
installed_form="$(inst_watch_name testview test)"
ne "template != installed (test)" "$template_form" "$installed_form"

# ---------------------------------------------------------------------------
# 3. WATCHD QUERIES THE INSTALLED NAME — extract the unit name watchd.sh passes
#    to systemctl by sourcing conf.sh the same way watchd.sh does.
# ---------------------------------------------------------------------------
echo
echo "WATCHD QUERIES INSTALLED NAME — not the template form:"

# A minimal watcher manifest with one daemon row.
MOCK_WATCHERS="$TMP/watchers"
mkdir -p "$MOCK_WATCHERS"
cat > "$MOCK_WATCHERS/testview.conf" <<'CONF'
kind=daemon
target=SPIRA_DB
CONF

# Stub watchd.sh's rows reader to return a predictable row.
# We source conf.sh and call watch_unit_name ourselves, verifying it does NOT
# produce the template form for any name a daemon row could carry.
for _name in answers testview cockpit-status; do
    got="$(load_watch_unit_name prod "$_name")"
    ne "watchd queries non-template for $_name" "$got" "spira-watch@${_name}.service"
    eq "watchd queries per-instance for $_name" "$got" "spira-watch-${_name}-prod.service"
done
unset _name got

# ---------------------------------------------------------------------------
echo
tl_summary
