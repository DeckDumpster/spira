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
cat > "$TMP/bin/ps" <<'STUB'
#!/usr/bin/env bash
cat "$STUB_PS"
STUB
chmod +x "$TMP/bin/"*
echo '0::/user.slice/spira-aeon-ifrit-1.service' > "$TMP/proc/4242/cgroup"
echo '0::/user.slice/spira-aeon-ifrit-1.service' > "$TMP/proc/4343/cgroup"

export STUB_UNITS="$TMP/units" STUB_PS="$TMP/ps"
printf 'spira-aeon-ifrit-1.service loaded active running aeon\n' > "$TMP/units"
claim() { echo 4242 > "$TMP/run/aeon-$2-$1.pid"; printf '%s' "$3" > "$TMP/run/aeon-$2-$1.name"; }
claim sp-test1 guardian ifrit
LEDGER="$TMP/run/aeon-ledger.log"
ledger_at() { printf '%s %s\n' "$(date -u -d "$1" +%Y-%m-%dT%H:%M:%SZ)" "$2" >> "$LEDGER"; }
: > "$LEDGER"; : > "$TMP/ps"
: > "$TMP/run/sp-test1.log"

run() {
    tl_config SPIRA_RUN="$TMP/run"
    env -i PATH="$TMP/bin:$PATH" HOME="$TMP" SPIRA_RUN="$TMP/run" AEON_WATCH_PROC="$TMP/proc" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        STUB_UNITS="$STUB_UNITS" STUB_PS="$STUB_PS" \
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

for i in 1 2 3 4; do ledger_at '-1 min' 'awake builder idle'; done
ledger_at '-30 min' 'awake builder idle'
case "$(run --show)" in *CHURN*) bad=1 ;; *) bad=0 ;; esac
is "CHURN stays silent below 5 inside the window" "0" "$bad"
ledger_at '-1 min' 'awake builder idle'
want "CHURN fires at 5" "CHURN 5" "$(run --show)"
: > "$LEDGER"
for i in 1 2 3; do ledger_at '-1 min' 'reopened builder sp-x fast-tier-red'; done
out="$(run --show)"
want "REOPENS fires at 3" "REOPENS 3" "$out"
case "$out" in *CHURN*) bad=1 ;; *) bad=0 ;; esac
is "REOPENS alone does not raise CHURN" "0" "$bad"

rm -f "$TMP"/run/aeon-*-sp-test1.*; claim xx-test2 guardian ifrit
: > "$TMP/run/xx-test2.log"; touch -d '-40 min' "$TMP/run/xx-test2.log"
tl_config SPIRA_ID_PREFIX=xx
out="$(run --show)"
tl_config SPIRA_ID_PREFIX=sp
want "a non-sp prefix finds the claimed bead" "xx-test2" "$out"
want "and its stalled log is flagged" "STALL" "$out"

rm -f "$TMP"/run/aeon-*.pid
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
