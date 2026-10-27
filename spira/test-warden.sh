#!/usr/bin/env bash
#
# test-warden.sh — the warden is called on a schedule, files exactly one sweep at a time,
# and its partition never reaches plan beads.
#
# tier: T1
# covers: spira/warden-trigger.sh spira/runes/warden.* spira/conf.d/SPIRA_WARDEN_LABEL systemd/spira-warden.* install/src/manifest.rs
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
    list) [ -n "${BD_LIST_FAIL:-}" ] && exit 124; printf '%s\n' "${BD_LIST_OUTPUT:-[]}" ;;
    create) [ -n "${BD_CREATE_FAIL:-}" ] && exit 1; echo sp-new1 ;;
esac
exit 0
STUB
chmod +x "$STUB"

# Whether a sweep is still open is its lifecycle row's answer, never bd status (sp-mve9i):
# a sweep is filed work the warden's dwarf claims and delivers, so the machine holds its
# state. bd only says which beads carry the labels; the rows live in this stand-in spira-lc.
lc_fix_init "$T/lc"

run_trigger() {
    tl_config SPIRA_DB="$T/fixture.db" SPIRA_RUN="$T/run" SPIRA_REPO_MAP="$MAP" SPIRA_BD="$STUB"
    # EXTRA_ENV: a registered key (e.g. SPIRA_WARDEN_LABEL) goes to tl_config too; anything
    # else stays a plain env assignment for the env -i call below.
    local extra_env=() kv k
    for kv in ${EXTRA_ENV:-}; do
        k="${kv%%=*}"
        if [ -f "$HERE/conf.d/$k" ]; then tl_config "$kv"; else extra_env+=("$kv"); fi
    done
    env -i HOME="$T" PATH="$HERE:${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" \
        SPIRA_CONF="$T/none.conf" BD_LOG_PATH="$LOG" \
        BD_LIST_OUTPUT="${BD_LIST_OUTPUT:-[]}" BD_CREATE_FAIL="${BD_CREATE_FAIL:-}" BD_LIST_FAIL="${BD_LIST_FAIL:-}" \
        SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" \
        SPIRA_WARDEN_ATTEMPTS="${SPIRA_WARDEN_ATTEMPTS:-3}" SPIRA_WARDEN_BACKOFF=0 \
        "${extra_env[@]}" warden-trigger.sh 2>&1
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
want "sweep has a READY lifecycle row at birth" "create-bead sp-new1" "$(cat "$LC_FIX/creates.log")"
is "row is READY" 1 "$([ -f "$LC_FIX/bead/READY/sp-new1" ] && echo 1 || echo 0)"
rm -f "$LC_FIX/bead/READY/sp-new1"

echo; echo "FILING: a failed row creation is an error naming the bead"
: > "$LOG"; : > "$LC_FIX/creates.log"; touch "$LC_FIX/refuse-create"
out="$(run_trigger)"; rc=$?
is "row failure exits 1" 1 "$rc"
want "names the unclaimable bead" "sp-new1" "$out"
is "row creation is retried ATTEMPTS times" 3 "$(grep -c "create-bead sp-new1" "$LC_FIX/creates.log")"
rm -f "$LC_FIX/refuse-create"

echo; echo "FILING: a row creation that fails once is retried and succeeds"
: > "$LOG"; : > "$LC_FIX/creates.log"; touch "$LC_FIX/refuse-create-once"
out="$(run_trigger)"; rc=$?
is "transient row failure exits 0" 0 "$rc"
is "create-bead called twice" 2 "$(grep -c 'create-bead sp-new1' "$LC_FIX/creates.log")"
is "row is READY after retry" 1 "$([ -f "$LC_FIX/bead/READY/sp-new1" ] && echo 1 || echo 0)"
is "bead filed once" 1 "$(grep -c ' create' "$LOG")"
rm -f "$LC_FIX/bead/READY/sp-new1"

echo; echo "DEDUP: an open sweep suppresses filing (positive control: filing happened above)"
: > "$LOG"
lc_bead READY sp-x deadbeef 0
out="$(BD_LIST_OUTPUT='[{"id":"sp-x"}]' run_trigger)"; rc=$?
is "dedup exits 0" 0 "$rc"
lack "no second bead" "create" "$(cat "$LOG")"

echo; echo "DEDUP: a sweep the machine has seen handed on no longer suppresses filing"
: > "$LOG"
lc_bead LANDED sp-x deadbeef 0
out="$(BD_LIST_OUTPUT='[{"id":"sp-x"}]' run_trigger)"; rc=$?
is "landed sweep: files, exits 0" 0 "$rc"
want "landed sweep: a new sweep is filed" "create" "$(cat "$LOG")"
rm -f "$LC_FIX"/bead/*/sp-x "$LC_FIX/show/sp-x"

echo; echo "DEDUP: a poisoned sweep is dead and no longer suppresses filing"
: > "$LOG"
lc_bead READY sp-x deadbeef 0
printf '{"bead_id":"sp-x","state":"READY","holds":["poison"]}' > "$LC_FIX/bead/READY/sp-x"
out="$(BD_LIST_OUTPUT='[{"id":"sp-x"}]' run_trigger)"; rc=$?
is "poisoned sweep: files, exits 0" 0 "$rc"
want "poisoned sweep: a fresh sweep is filed" "create" "$(cat "$LOG")"
rm -f "$LC_FIX"/bead/*/sp-x "$LC_FIX/show/sp-x"

echo; echo "FAILURE: a create that fails exits 1"
out="$(BD_CREATE_FAIL=1 run_trigger)"; rc=$?
is "create failure exits 1" 1 "$rc"

echo; echo "RETRY: a failed create is retried before giving up"
: > "$LOG"
out="$(BD_CREATE_FAIL=1 run_trigger)"
is "create attempted on every attempt" 3 "$(grep -c '^-C .* create\|^create' "$LOG")"
want "attempts are logged" "attempt 2/3" "$out"

echo; echo "RETRY: an unreadable count is retried, then refuses to file"
: > "$LOG"
out="$(BD_LIST_FAIL=1 run_trigger)"; rc=$?
is "count failure exits 0" 0 "$rc"
want "count retried" "attempt 3/3" "$out"
lack "nothing filed" "create" "$(cat "$LOG")"

echo; echo "LABEL: configured non-default label is used"
: > "$LOG"
out="$(EXTRA_ENV="SPIRA_WARDEN_LABEL=custom-watch" run_trigger)"
want "custom label in create args" "custom-watch" "$(cat "$LOG")"
lack "default label absent when overridden" "warden-sweep" "$(cat "$LOG")"
tl_config SPIRA_WARDEN_LABEL=warden-sweep

echo; echo "PARTITION: warden claims only its sweep, never plan work"
export SPIRA_HOME="$HERE" SPIRA_CONF="$T/none.conf"
# SPIRA_RUNES_DIR no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at the real runes rune_get below reads from.
SPIRA_RUN="$T/run"; tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_RUNES_DIR="$HERE/runes"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
wl="$(rune_get warden RUNE_LABELS)"
want "warden labels include the warden label" "warden-sweep" "$wl"
lack "warden labels exclude plan" "plan" "$wl"
lack "warden labels exclude incident" "incident" "$wl"
lack "builder labels exclude warden" "warden-sweep" "$(rune_get builder RUNE_LABELS)"
is "warden lane" "warden" "$(rune_get warden RUNE_LANE)"
want "warden lane is declared" "warden" "${SPIRA_LANES:-}"
want "brief forbids claiming plan beads" "Never claim a plan bead" "$(cat "$HERE/runes/warden.md")"
want "brief files follow-ups through the contract" "work file" "$(cat "$HERE/runes/warden.md")"

echo
tl_summary
