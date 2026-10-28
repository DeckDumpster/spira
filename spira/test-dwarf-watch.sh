#!/usr/bin/env bash
#
# test-dwarf-watch.sh — dwarf-watch.sh names every live dwarf, fires each of STALL, GATE, TESTENV,
# CHURN and REOPENS on a crafted fleet, stays silent on a healthy one, and never prints an argv.
#
# tier: T1
# covers: spira/dwarf-watch.sh spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-dwarf-watch.sh"
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
echo '0::/user.slice/spira-dwarf-eitri-1.service' > "$TMP/proc/4242/cgroup"
echo '0::/user.slice/spira-dwarf-eitri-1.service' > "$TMP/proc/4343/cgroup"

export STUB_UNITS="$TMP/units" STUB_PS="$TMP/ps"
printf 'spira-dwarf-eitri-1.service loaded active running dwarf\n' > "$TMP/units"
claim() { echo 4242 > "$TMP/run/dwarf-$2-$1.pid"; printf '%s' "$3" > "$TMP/run/dwarf-$2-$1.name"; }
claim sp-test1 warden eitri
LEDGER="$TMP/run/dwarf-ledger.log"
ledger_at() { printf '%s %s\n' "$(date -u -d "$1" +%Y-%m-%dT%H:%M:%SZ)" "$2" >> "$LEDGER"; }
: > "$LEDGER"; : > "$TMP/ps"
: > "$TMP/run/sp-test1.log"

run() {
    tl_config SPIRA_RUN="$TMP/run"
    env -i PATH="$TMP/bin:$PATH" HOME="$TMP" SPIRA_RUN="$TMP/run" DWARF_WATCH_PROC="$TMP/proc" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        STUB_UNITS="$STUB_UNITS" STUB_PS="$STUB_PS" \
        bash "$HERE/dwarf-watch.sh" "$@" 2>&1
}

out="$(run --show)"
want "positive control: the live dwarf is named with its bead" "warden/eitri sp-test1" "$out"
want "the live count is reported" "dwarves live: 1" "$out"
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
want "the unit is named" "spira-dwarf-eitri-1.service" "$out"
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

rm -f "$TMP"/run/dwarf-*-sp-test1.*; claim xx-test2 warden eitri
: > "$TMP/run/xx-test2.log"; touch -d '-40 min' "$TMP/run/xx-test2.log"
tl_config SPIRA_ID_PREFIX=xx
out="$(run --show)"
tl_config SPIRA_ID_PREFIX=sp
want "a non-sp prefix finds the claimed bead" "xx-test2" "$out"
want "and its stalled log is flagged" "STALL" "$out"

rm -f "$TMP"/run/dwarf-*.pid
want "an unclaimed dwarf is not a stall" "no claim seen yet" "$(run --show)"

: > "$TMP/units"
want "no live dwarves is stated, not silent" "dwarves live: 0" "$(run --show)"

run watch --interval 7 --ticks 1 >/dev/null
want "watch writes a health record" "ok " "$(cat "$TMP/run/watchd/dwarf-watch.health")"
run health >/dev/null; is "health passes after a fresh poll" "0" "$?"
echo "ok 1 100 7" > "$TMP/run/watchd/dwarf-watch.health"
run health >/dev/null; is "health fails once the last poll is stale" "1" "$?"

grep -q '^dwarf-watch|daemon|' "$HERE/watchers" && ok=0 || ok=1
is "watchers manifest carries the dwarf-watch row" "0" "$ok"

tl_summary
