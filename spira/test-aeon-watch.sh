#!/usr/bin/env bash
#
# test-aeon-watch.sh — aeon-watch.sh names every live aeon, fires each of STALL, GATE, TESTENV,
# CHURN and REOPENS on a crafted fleet, stays silent on a healthy one, and never prints an argv.
#
# tier: T1
# covers: spira/aeon-watch.sh spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-aeon-watch.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/bin" "$TMP/run" "$TMP/proc/4242" "$TMP/proc/4343"

cat > "$TMP/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
case "$*" in
    *list-units*) cat "$STUB_UNITS" ;;
    *show*) date -u -d '-30 min' '+%a %Y-%m-%d %H:%M:%S UTC' ;;
esac
STUB
cat > "$TMP/bin/journalctl" <<'STUB'
#!/usr/bin/env bash
case "$*" in
    *" -u "*) cat "$STUB_CLAIMS" ;;
    *) cat "$STUB_JOURNAL" ;;
esac
STUB
cat > "$TMP/bin/ps" <<'STUB'
#!/usr/bin/env bash
cat "$STUB_PS"
STUB
chmod +x "$TMP/bin/"*
echo '0::/user.slice/spira-aeon-ifrit-1.service' > "$TMP/proc/4242/cgroup"
echo '0::/user.slice/spira-aeon-ifrit-1.service' > "$TMP/proc/4343/cgroup"

export STUB_UNITS="$TMP/units" STUB_CLAIMS="$TMP/claims" STUB_JOURNAL="$TMP/journal" STUB_PS="$TMP/ps"
printf 'spira-aeon-ifrit-1.service loaded active running aeon\n' > "$TMP/units"
printf 'guardian/ifrit: claimed sp-test1\n' > "$TMP/claims"
: > "$TMP/journal"; : > "$TMP/ps"
: > "$TMP/run/sp-test1.log"

run() {
    tl_config SPIRA_RUN="$TMP/run"
    env -i PATH="$TMP/bin:$PATH" HOME="$TMP" SPIRA_RUN="$TMP/run" AEON_WATCH_PROC="$TMP/proc" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        STUB_UNITS="$STUB_UNITS" STUB_CLAIMS="$STUB_CLAIMS" STUB_JOURNAL="$STUB_JOURNAL" STUB_PS="$STUB_PS" \
        bash "$HERE/aeon-watch.sh" "$@" 2>&1
}

out="$(run --show)"
want "positive control: the live aeon is named with its bead" "guardian/ifrit sp-test1" "$out"
want "the live count is reported" "aeons live: 1" "$out"
for f in STALL GATE TESTENV CHURN REOPENS "no claim"; do
    case "$out" in *"$f"*) bad=1 ;; *) bad=0 ;; esac
    is "healthy fleet: no $f" "0" "$bad"
done

touch -d '-40 min' "$TMP/run/sp-test1.log"
want "STALL fires on an old log" "STALL" "$(run --show)"

printf '4242 1000 gate\n4343 2000 testenv\n9999 5000 dolt\n' > "$TMP/ps"
out="$(run --show)"
want "GATE fires on a gate over 5m" "GATE pid 4242 running 16m" "$out"
want "TESTENV fires on testenv over 15m" "TESTENV pid 4343 running 33m" "$out"
want "the unit is named" "spira-aeon-ifrit-1.service" "$out"
case "$out" in *9999*) bad=1 ;; *) bad=0 ;; esac
is "an unmatched comm is not reported" "0" "$bad"

printf '4242 200 gate\n4343 800 testenv\n' > "$TMP/ps"
out="$(run --show)"
case "$out" in *GATE*|*TESTENV*) bad=1 ;; *) bad=0 ;; esac
is "short gate and testenv runs stay silent" "0" "$bad"

for i in 1 2 3 4 5; do echo 'builder: nothing ready to claim'; done > "$TMP/journal"
want "CHURN fires at 5" "CHURN 5" "$(run --show)"
for i in 1 2 3; do echo 'REOPENED — fast tier red, handoff refused'; done > "$TMP/journal"
out="$(run --show)"
want "REOPENS fires at 3" "REOPENS 3" "$out"
case "$out" in *CHURN*) bad=1 ;; *) bad=0 ;; esac
is "REOPENS alone does not raise CHURN" "0" "$bad"

: > "$TMP/claims"
want "an unclaimed aeon is not a stall" "no claim seen yet" "$(run --show)"

: > "$TMP/units"
want "no live aeons is stated, not silent" "aeons live: 0" "$(run --show)"

run watch --interval 7 --ticks 1 >/dev/null
want "watch writes a health record" "ok " "$(cat "$TMP/run/watchd/aeon-watch.health")"
run health >/dev/null; is "health passes after a fresh poll" "0" "$?"
echo "ok 1 100 7" > "$TMP/run/watchd/aeon-watch.health"
run health >/dev/null; is "health fails once the last poll is stale" "1" "$?"

grep -q '^aeon-watch|daemon|' "$HERE/watchers" && ok=0 || ok=1
is "watchers manifest carries the aeon-watch row" "0" "$ok"

tl_summary
