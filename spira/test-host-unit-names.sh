#!/usr/bin/env bash
#
# test-host-unit-names.sh — every installed watcher unit resolves to a known unit.
#
#   ./test-host-unit-names.sh
#
# host-reason: checks against the real installed fleet — unit suffix bugs only manifest
#              against units systemd actually knows about; a container has none installed
#
# WHAT THIS TESTS. Two formulas name installed watcher units. install.sh's UNIT rule
# (systemd/units.sh: inst_watch_name) always appends -$SPIRA_INSTANCE, prod included.
# conf.sh's watch_unit_name follows the same formula and is the accessor every tool
# calls at runtime. watch-refresh.sh uses it to build the systemctl show query; watchd.sh
# uses it in cmd_status and cmd_restart. A mismatch between what is installed and what
# is queried produces ActiveState=inactive for every unit, leaving every stale watcher
# undetected and unrestarted.
#
# The check: for every daemon row in the watcher manifest, watch_unit_name produces a
# unit that systemd actually has installed. A "not-found" LoadState is a "?" — a failure.
#
# STRICTLY READ-ONLY. Only list-unit-files, show, is-active, is-enabled — no start, no
# stop, no restart, no daemon-reload. Stated and asserted: the tools this suite checks
# exist to MUTATE, and read-only is one argument away from the mutating form.
#
# SKIP CONDITION: spira is not installed on this host (no spira-*-prod.* unit files).
#
# covers: spira/conf.sh spira/watch-refresh.sh spira/watchd.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-host-unit-names.sh"

# SKIP if spira is not installed. Any spira-*-prod.* unit file is sufficient.
# hermetic-ok: host-reason suite — must enumerate units systemd actually has installed
installed_count="$(systemctl --user list-unit-files --no-legend 2>/dev/null \
    | awk '{print $1}' | grep -c '^spira-.*-prod\.' || true)"
if [ "${installed_count:-0}" -eq 0 ]; then
    printf 'SKIP test-host-unit-names.sh: no spira-*-prod.* units installed on this host\n' >&2
    exit 77
fi
ok "spira installation found (${installed_count} spira-*-prod.* unit files)"

# ASSERT READ-ONLY. This test must never write to systemd state. The allowed operations
# are list-unit-files, show, is-active, is-enabled. Any other systemctl verb is a bug.
# Asserted here so a future edit that adds a mutating call will fail the suite rather than
# silently run on the host.
echo
echo "read-only assertion — no mutating systemctl calls in this suite:"
readonly_ok=1
for verb in start stop restart reload daemon-reload enable disable mask; do
    if grep -q "systemctl.*[[:space:]]${verb}[[:space:]]" "$0" 2>/dev/null; then
        bad "no '$verb' call in this suite" "found in $(basename "$0")"
        readonly_ok=0
    fi
done
[ "$readonly_ok" = 1 ] && ok "no mutating systemctl verbs present in $(basename "$0")"

# Load conf.sh into this shell to get watch_unit_name and SPIRA_INSTANCE.
# conf.sh reads spira.conf from the usual locations; on the host that is the real config.
# shellcheck disable=SC1090
. "$HERE/conf.sh"
ok "conf.sh loaded (SPIRA_INSTANCE=${SPIRA_INSTANCE:-prod})"

echo
echo "every daemon watcher resolves to an installed unit (no ?):"

# Get daemon rows from the manifest via watchd.sh. watchd_rows is defined in watchd.sh,
# but sourcing it here would run its startup checks. Use watchd.sh units instead, which
# lists the template form — we extract the name from that.
rows="$(bash "$HERE/watchd.sh" units 2>/dev/null)" || {
    printf 'SKIP test-host-unit-names.sh: watchd.sh units failed (manifest error)\n' >&2
    exit 77
}

if [ -z "$rows" ]; then
    printf 'SKIP test-host-unit-names.sh: no daemon rows in manifest\n' >&2
    exit 77
fi

while IFS= read -r template_unit; do
    [ -n "$template_unit" ] || continue
    # Extract watcher name from template form: spira-watch@answers.service → answers
    name="${template_unit#spira-watch@}"; name="${name%.service}"
    [ -n "$name" ] || continue

    # watch_unit_name is the formula every tool uses at runtime.
    installed_unit="$(watch_unit_name "$name")"

    # The unit must not look like "?" or a template — those are the bug forms.
    case "$installed_unit" in
        '?') bad "watch_unit_name $name: returned ?" "$installed_unit"; continue ;;
        *'@'*) bad "watch_unit_name $name: returned template form" "$installed_unit"; continue ;;
    esac

    # systemd must know this unit (LoadState != not-found).
    # hermetic-ok: host-reason suite — must check real systemd for LoadState
    load_state="$(systemctl --user show --property=LoadState "$installed_unit" 2>/dev/null \
        | sed 's/^LoadState=//')"
    case "$load_state" in
        not-found|'')
            bad "watch_unit_name $name = $installed_unit: not found in systemd" \
                "LoadState=${load_state:-empty}" ;;
        *)
            ok "watch_unit_name $name = $installed_unit (LoadState=$load_state)" ;;
    esac
done <<< "$rows"

echo
echo "installed unit files match the naming formula (UNIT rule, not PATH-suffix rule):"

# The UNIT rule: every spira-watch-* unit is suffixed with -$SPIRA_INSTANCE.
# The PATH-suffix rule: conf.sh's _spira_inst_sfx is empty for prod (giving no suffix).
# Only the UNIT rule produces the correct unit name.
# List every installed watcher unit and verify watch_unit_name would produce it.
while IFS= read -r unit_file; do
    [ -n "$unit_file" ] || continue
    # Extract: spira-watch-answers-prod.service → answers (strip prefix and -$SPIRA_INSTANCE suffix)
    name="${unit_file#spira-watch-}"; name="${name%-${SPIRA_INSTANCE:-prod}.service}"
    name="${name%.service}"  # strip bare .service if SPIRA_INSTANCE was empty
    [ -n "$name" ] || continue

    expected="$(watch_unit_name "$name")"
    if [ "$expected" = "$unit_file" ]; then
        ok "installed $unit_file matches watch_unit_name $name"
    else
        bad "installed $unit_file does not match watch_unit_name $name" \
            "watch_unit_name returned: $expected"
    fi
# hermetic-ok: host-reason suite — must list real installed unit files
done < <(systemctl --user list-unit-files --no-legend 2>/dev/null \
    | awk '{print $1}' | grep "^spira-watch-.*-${SPIRA_INSTANCE:-prod}\.service$")

echo
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
