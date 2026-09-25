#!/usr/bin/env bash
#
# test-testenv-image-heartbeat.sh — a cold build must not be silent for longer than the
# heartbeat interval, and a build that dies of disk exhaustion must say so.
#
# WHY THIS EXISTS. Batch PR silence on "Acquire the test image" and a genuine hang looked
# identical from outside: the cold build compiles bd and its Go/Dolt dependencies from
# source and printed nothing between the module download and the finished binary. A
# heartbeat that fires whether or not the build itself is talkative is the fix; this suite
# requires it to actually fire, not just exist as code nobody calls.
#
# FIXTURES. podman is stubbed on PATH so no real image is built. The stub's `build`
# subcommand sleeps (configurable) and can be made to fail with a disk-exhaustion message,
# which is what a real `podman build` prints when the underlying `cp`/`tar` runs out of
# space.
#
# covers: spira/testenv.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-testenv-image-heartbeat.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
FIXTURE="$TMP/spira"
mkdir -p "$FIXTURE/testenv" "$TMP/bin"
cp "$HERE/testenv.sh"            "$FIXTURE/testenv.sh"
cp "$HERE/conf.sh"               "$FIXTURE/conf.sh"
cp "$HERE/deps.toml"             "$FIXTURE/deps.toml"
cp "$HERE/testenv/Containerfile" "$FIXTURE/testenv/Containerfile"
PIN="$TMP/bd-pin"; printf 'BD_PIN_MIGRATIONS=42\n' > "$PIN"

# podman stub: `image exists` always misses (forces the build path), no registry is
# configured so no pull is attempted, and `build` sleeps STUB_BUILD_SLEEP seconds before
# exiting STUB_BUILD_RC. A failing build prints STUB_BUILD_ERR to stdout, the way podman's
# own build prints the underlying command's error onto the step it was running.
cat > "$TMP/bin/podman" <<'STUB'
#!/usr/bin/env bash
case "$1" in
    image) exit 1 ;;
    build)
        echo "STEP 1/9: FROM docker.io/library/ubuntu:24.04"
        sleep "${STUB_BUILD_SLEEP:-0}"
        if [ "${STUB_BUILD_RC:-0}" != "0" ]; then
            printf '%s\n' "${STUB_BUILD_ERR:-build failed}"
            exit "${STUB_BUILD_RC}"
        fi
        exit 0
        ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/podman"

# run <heartbeat_s> <sleep_s> <rc> <err> -> stderr of `testenv.sh image` in $ERR
ERR="$TMP/stderr.log"
run() {
    local hb="$1" slp="$2" rc="$3" err="$4"
    PATH="$TMP/bin:$PATH" SPIRA_BD_PIN="$PIN" \
    SPIRA_TESTENV_BUILD_HEARTBEAT="$hb" \
    STUB_BUILD_SLEEP="$slp" STUB_BUILD_RC="$rc" STUB_BUILD_ERR="$err" \
        bash "$FIXTURE/testenv.sh" image >/dev/null 2>"$ERR"
}

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "positive control — a build shorter than the heartbeat interval emits no heartbeat:"
# ──────────────────────────────────────────────────────────────────────────────
# Proves the heartbeat is not printed unconditionally on every build (which would make
# the "heartbeat fired" assertion below meaningless).
run 5 0 0 ""
hb_count=$(grep -c 'testenv: building — ' "$ERR" 2>/dev/null || true)
[ "${hb_count:-0}" -eq 0 ] && ok "no heartbeat line when the build finishes inside one interval" \
    || bad "no heartbeat line when the build finishes inside one interval" "saw $hb_count"

echo
echo "the step announces a COLD build before it starts, and why:"
grep -q 'COLD build' "$ERR" && ok "COLD build is announced" \
    || bad "COLD build is announced" "no [COLD build] line"
grep -q 'Containerfile' "$ERR" && ok "the closure file is named" \
    || bad "the closure file is named" "no [Containerfile] in the announcement"
grep -qE 'minutes' "$ERR" && ok "a typical cold-build duration is stated" \
    || bad "a typical cold-build duration is stated" "no duration mentioned"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "SEEN RED then SEEN GREEN — a build that outlasts the heartbeat interval heartbeats:"
# ──────────────────────────────────────────────────────────────────────────────
# RED: with the interval longer than the build, no heartbeat can appear (re-asserts the
# positive control above under the exact knobs the GREEN case flips).
run 30 2 0 ""
red_count=$(grep -c 'testenv: building — ' "$ERR" 2>/dev/null || true)
[ "${red_count:-0}" -eq 0 ] && ok "SEEN RED: interval longer than the build produces no heartbeat" \
    || bad "SEEN RED: interval longer than the build produces no heartbeat" "saw $red_count"

# GREEN: shrink the interval below the build's sleep. The build must now heartbeat at
# least once before it finishes.
run 1 3 0 ""
green_count=$(grep -c 'testenv: building — ' "$ERR" 2>/dev/null || true)
[ "${green_count:-0}" -ge 1 ] && ok "SEEN GREEN: a build outlasting the interval heartbeats ($green_count line(s))" \
    || bad "SEEN GREEN: a build outlasting the interval heartbeats" "saw $green_count"

grep 'testenv: building — ' "$ERR" | grep -q 'elapsed' \
    && ok "the heartbeat line carries elapsed time" \
    || bad "the heartbeat line carries elapsed time" "no [elapsed] in heartbeat line"
grep 'testenv: building — ' "$ERR" | grep -qE 'disk .* free' \
    && ok "the heartbeat line carries free disk" \
    || bad "the heartbeat line carries free disk" "no [disk ... free] in heartbeat line"
grep 'testenv: building — ' "$ERR" | grep -qE 'memory .* free' \
    && ok "the heartbeat line carries free memory" \
    || bad "the heartbeat line carries free memory" "no [memory ... free] in heartbeat line"
grep 'testenv: building — ' "$ERR" | grep -q 'STEP' \
    && ok "the heartbeat line names the build stage" \
    || bad "the heartbeat line names the build stage" "no [STEP] in heartbeat line"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "SEEN RED then SEEN GREEN — disk exhaustion is named, not left to look like a hang:"
# ──────────────────────────────────────────────────────────────────────────────
# RED: a generic build failure does NOT get misreported as disk exhaustion.
run 30 0 1 "some unrelated compile error"
grep -qi 'disk exhausted' "$ERR" \
    && bad "SEEN RED: a generic failure is not misreported as disk exhaustion" "[disk exhausted] appeared" \
    || ok "SEEN RED: a generic failure is not misreported as disk exhaustion"
grep -q 'image build failed' "$ERR" \
    && ok "a generic failure still reports the build failed" \
    || bad "a generic failure still reports the build failed" "no failure line"

# GREEN: an ENOSPC-shaped error from the build IS named as disk exhaustion.
run 30 0 1 "write /var/lib/containers/foo: no space left on device"
grep -qi 'disk exhausted' "$ERR" \
    && ok "SEEN GREEN: ENOSPC is reported as disk exhaustion" \
    || bad "SEEN GREEN: ENOSPC is reported as disk exhaustion" "no [disk exhausted] line"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
