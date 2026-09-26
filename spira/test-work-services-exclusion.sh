#!/usr/bin/env bash
#
# test-work-services-exclusion.sh — world.sh work_services() excludes the
# instance-qualified cockpit and Loom service names, not only their bare
# pre-migration forms.
#
# THREE PROPERTIES, each with its positive control (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL: the old exclusion pattern (^spira-cockpit\.service$) does NOT
#      match spira-cockpit-prod.service — proving the defect and proving the new assertion
#      cannot be met by the old code. An assertion that passes both old and new code is not
#      evidence of the fix (sp-4biz survived exactly that kind of check).
#
#   2. work_services() — called by world.sh stop — excludes both cockpit and Loom when
#      their names carry the instance suffix. Work units (sentinel, landing) are preserved.
#
#   3. world.sh stop does not pass cockpit or Loom to systemctl stop. The pair survives a
#      stop/start cycle, which is the operator-visible property the comment in world.sh
#      promises.
#
# SYSTEMCTL IS STUBBED — no real units are touched.
#
# defect: sp-xfg4
# tier: T1
# covers: spira/world.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-work-services-exclusion.sh"
TMP="$(mktemp -d)"
export STOPPED="$TMP/stopped.log"
: > "$STOPPED"
trap 'rm -rf "$TMP"' EXIT INT TERM

# STUB SYSTEMCTL. For list-units spira-*.service it returns a realistic mix that
# includes the instance-qualified cockpit and Loom units; for stop it records the
# target in STOPPED; for is-enabled/is-active timer probes it returns success so
# TIMERS is populated without real systemd. Everything else exits 0 silently.
BIN="$TMP/bin"; mkdir -p "$BIN"
cat > "$BIN/systemctl" <<'STUB'
#!/usr/bin/env bash
shift   # --user
subcmd="${1:-}"; shift || true
case "$subcmd" in
  list-units)
    # Emit nothing for watcher or timer queries; the service mix for service queries.
    [[ "${*}" == *watch* ]] && exit 0
    [[ "${*}" == *timer*  ]] && exit 0
    printf '%s loaded active running -\n' \
      spira-sentinel-prod.service \
      spira-landing.service \
      spira-cockpit-prod.service \
      spira-loom-prod.service
    ;;
  list-unit-files) exit 0 ;;
  stop) printf '%s\n' "$@" >> "$STOPPED" ;;
  is-enabled|is-active)
    case "${1:-}" in
      spira-sentinel-prod.timer|spira-ops-prod.timer|\
      spira-watchtower-prod.timer|spira-archivist-prod.timer|\
      spira-archive-prod.timer|spira-skew-prod.timer) exit 0 ;;
      *) exit 1 ;;
    esac ;;
esac
exit 0
STUB
chmod +x "$BIN/systemctl"

export SPIRA_SYSTEMCTL="$BIN/systemctl"
export SPIRA_INSTANCE=prod
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"

# ---------------------------------------------------------------------------
# 1. POSITIVE CONTROL: old exclusion pattern lets instance-qualified names through.
#
# The grep pattern that existed before this fix only excluded the bare name
# (^spira-cockpit\.service$). Feed the same unit list the stub returns and show
# that the old pattern includes cockpit-prod and loom-prod in its output — the bug.
# This proves the assertions in section 2 and 3 cannot be met by the old code.
# ---------------------------------------------------------------------------
echo
echo "1. positive control — old pattern lets instance-qualified names through:"

UNITS="spira-sentinel-prod.service
spira-landing.service
spira-cockpit-prod.service
spira-loom-prod.service"

old_out="$(printf '%s\n' "$UNITS" | grep -Ev '^spira-cockpit\.service$|^spira-watch@')"
want "old pattern lets cockpit-prod through (confirms bug)" "spira-cockpit-prod.service" "$old_out"
want "old pattern lets loom-prod through (confirms bug)"   "spira-loom-prod.service"    "$old_out"

# ---------------------------------------------------------------------------
# 2. world.sh stop — which calls work_services() internally — does not pass
#    cockpit or Loom to systemctl stop. Work units (sentinel timer, landing) are
#    still stopped, confirming work_services() still enumerates them.
# ---------------------------------------------------------------------------
echo
echo "2. world.sh stop does not stop cockpit or Loom:"

: > "$STOPPED"
"$HERE/world.sh" stop --why "test-work-services-exclusion" > "$TMP/stop.out" 2>&1 || true
stopped="$(cat "$STOPPED")"

nowant "cockpit-prod not stopped"       "spira-cockpit-prod.service" "$stopped"
nowant "loom-prod not stopped"          "spira-loom-prod.service"    "$stopped"
want   "sentinel timer was stopped"     "spira-sentinel-prod.timer"  "$stopped"

# ---------------------------------------------------------------------------
# 3. stop/start CYCLE: cockpit and Loom survive. After world.sh stop followed
#    by world.sh start, neither service appears in the stopped log — start does
#    not restart them (they are not TIMERS) because stop never killed them.
# ---------------------------------------------------------------------------
echo
echo "3. stop/start cycle — cockpit and Loom survive:"

: > "$STOPPED"
"$HERE/world.sh" stop --why "test-cycle-stop"  > "$TMP/stop2.out"  2>&1 || true
"$HERE/world.sh" start                          > "$TMP/start.out"  2>&1 || true
stopped2="$(cat "$STOPPED")"

nowant "cockpit-prod absent from cycle-stop list" "spira-cockpit-prod.service" "$stopped2"
nowant "loom-prod absent from cycle-stop list"    "spira-loom-prod.service"    "$stopped2"

echo
tl_summary
