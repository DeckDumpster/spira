#!/usr/bin/env bash
#
# test-warden.sh — the warden is summoned on a schedule, files exactly one sweep at a time,
# and its partition never reaches plan beads.
#
# tier: T1
# covers: spira/warden-trigger.sh spira/chamber/warden.* spira/conf.d/SPIRA_WARDEN_LABEL systemd/spira-warden.* install/src/manifest.rs
# hermetic-ok: no real database; SPIRA_BD is a stub
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
SPIRA_CONFIG_DIR="$(command -v spira-config >/dev/null 2>&1 && dirname "$(command -v spira-config)" || true)"
. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"
MAP="$T/map"
printf 'test-repo | /tmp/test-repo | push | origin/main | | true | develop\n' > "$MAP"

STUB="$T/stub-bd"; LOG="$T/bd.log"
cat > "$STUB" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    list) printf '%s\n' "${BD_LIST_OUTPUT:-[]}" ;;
    create) [ -n "${BD_CREATE_FAIL:-}" ] && exit 1 ;;
esac
exit 0
STUB
chmod +x "$STUB"

run_trigger() {
    env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
        SPIRA_CONF="$T/none.conf" SPIRA_BD="$STUB" BD_LOG_PATH="$LOG" \
        BD_LIST_OUTPUT="${BD_LIST_OUTPUT:-[]}" BD_CREATE_FAIL="${BD_CREATE_FAIL:-}" \
        SPIRA_DB="$T/fixture.db" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$MAP" \
        ${EXTRA_ENV:-} warden-trigger.sh 2>&1
}

echo "test-warden.sh"

echo; echo "SCHEDULE: the timer exists and drives the trigger"
want "service runs warden-trigger.sh" "warden-trigger.sh" "$(cat "$HERE/../systemd/spira-warden.service")"
want "service is in the work plane" "Plane=work" "$(cat "$HERE/../systemd/spira-warden.service")"
want "timer repeats" "OnUnitActiveSec=" "$(cat "$HERE/../systemd/spira-warden.timer")"
want "timer drives the service" "Unit=spira-warden.service" "$(cat "$HERE/../systemd/spira-warden.timer")"
want "timer is in the work plane" "Plane=work" "$(cat "$HERE/../systemd/spira-warden.timer")"
want "install manifest carries the timer" "spira-warden.timer" "$(cat "$HERE/../install/src/manifest.rs")"

echo; echo "FILING: no open sweep files one, with the warden label"
: > "$LOG"
out="$(run_trigger)"; rc=$?
is "files, exits 0" 0 "$rc"
want "bd create called" "create" "$(cat "$LOG")"
want "carries warden label" "warden-sweep" "$(cat "$LOG")"

echo; echo "DEDUP: an open sweep suppresses filing (positive control: filing happened above)"
: > "$LOG"
out="$(BD_LIST_OUTPUT='[{"id":"sp-x"}]' run_trigger)"; rc=$?
is "dedup exits 0" 0 "$rc"
lack "no second bead" "create" "$(cat "$LOG")"

echo; echo "FAILURE: a create that fails exits 1"
out="$(BD_CREATE_FAIL=1 run_trigger)"; rc=$?
is "create failure exits 1" 1 "$rc"

echo; echo "LABEL: configured non-default label is used"
: > "$LOG"
out="$(EXTRA_ENV="SPIRA_WARDEN_LABEL=custom-watch" run_trigger)"
want "custom label in create args" "custom-watch" "$(cat "$LOG")"
lack "default label absent when overridden" "warden-sweep" "$(cat "$LOG")"

echo; echo "PARTITION: warden claims only its sweep, never plan work"
export SPIRA_HOME="$HERE" SPIRA_RUN="$T/run" SPIRA_CONF="$T/none.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
wl="$(fayth_get warden FAYTH_LABELS)"
want "warden labels include the warden label" "warden-sweep" "$wl"
lack "warden labels exclude plan" "plan" "$wl"
lack "warden labels exclude incident" "incident" "$wl"
lack "builder labels exclude warden" "warden-sweep" "$(fayth_get builder FAYTH_LABELS)"
is "warden lane" "warden" "$(fayth_get warden FAYTH_LANE)"
want "warden lane is declared" "warden" "${SPIRA_LANES:-}"
want "brief forbids claiming plan beads" "Never claim a plan bead" "$(cat "$HERE/chamber/warden.md")"
want "brief files follow-ups through the contract" "bead.sh file" "$(cat "$HERE/chamber/warden.md")"

echo
tl_summary
