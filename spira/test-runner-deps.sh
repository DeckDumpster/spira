#!/usr/bin/env bash
#
# test-runner-deps.sh — the host dependency check discriminates, and attributes.
#
#   ./test-runner-deps.sh
#
# WHAT THIS PROTECTS. runner-deps.sh decides whether a machine can run the suite
# corpus at all. Two ways for it to be wrong, and both are quiet:
#
#   It passes a machine that cannot run anything. Then testenv-batch.sh exits 2,
#   the gate maps that to 75 — "harness fault, run it again" — and every run
#   forever says retry while the real problem is a package nobody installed.
#
#   It fails a machine that can. Then a correct branch is refused for a reason
#   that is not in the branch.
#
# So the suite plants a broken runtime and requires the script to say so, and
# requires the same script to be silent on a machine that works. A check that
# only ever passes has demonstrated nothing.
#
# --check IS WHAT IS TESTED, never the installing path: that one calls apt-get
# and usermod, which a suite must not do to the machine it is running on.
#
# covers: spira/runner-deps.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
SCRIPT="$HERE/runner-deps.sh"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-runner-deps.sh"
echo

if [ ! -x "$SCRIPT" ]; then
    bad "runner-deps.sh is executable" "not found or not executable at $SCRIPT"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "runner-deps.sh is executable"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo
echo "it refuses a machine whose container runtime cannot start:"
# THE POSITIVE CONTROL. A podman that exits non-zero for everything is exactly
# the shape of a runtime that is installed and broken — the case that would
# otherwise reach testenv-batch.sh and be reported as a transient harness fault.
mkdir -p "$TMP/brokenbin"
printf '#!/bin/sh\nexit 1\n' > "$TMP/brokenbin/podman"
chmod +x "$TMP/brokenbin/podman"
out="$(PATH="$TMP/brokenbin:$PATH" bash "$SCRIPT" --check 2>&1)"; rc=$?
if [ "$rc" -eq 0 ]; then
    bad "a broken runtime is refused" "exited 0"
else
    ok "a broken runtime is refused (rc=$rc)"
fi
case "$out" in
    *MISSING*) ok "it names what is missing" ;;
    *)         bad "it names what is missing" "no MISSING line in output" ;;
esac
# THE EXIT CODE IS THE WHOLE POINT. 75 means "the harness broke, retry"; a host
# that is missing a package will be missing it on every retry, so this must be a
# plain failure attributed to the step that owns it.
if [ "$rc" -eq 75 ]; then
    bad "a host gap is not reported as a retryable harness fault" "exited 75"
else
    ok "a host gap is not reported as a retryable harness fault"
fi
case "$out" in
    *"not a fault in the branch"*) ok "it says the branch is not at fault" ;;
    *) bad "it says the branch is not at fault" "no attribution line" ;;
esac

echo
echo "it accepts a machine that works:"
# Only meaningful where the machine genuinely has a working rootless runtime. On
# a box without one this asserts nothing and says so, rather than reporting a
# pass it did not earn.
if command -v podman >/dev/null 2>&1 && podman info >/dev/null 2>&1; then
    if bash "$SCRIPT" --check >/dev/null 2>&1; then
        ok "a working machine passes"
    else
        bad "a working machine passes" "refused a host with a working rootless podman"
    fi
else
    printf '  skip  a working machine passes (no usable podman on this host)\n'
fi

echo
echo "--check changes nothing:"
# An assertion-only mode that installs would make the gate's first step a
# mutation of the machine under test, and a suite could not run it at all.
if grep -qE 'apt-get|usermod|enable-linger' "$SCRIPT"; then
    if grep -q 'CHECK_ONLY' "$SCRIPT"; then
        ok "the mutating calls are behind the check-only guard"
    else
        bad "the mutating calls are behind the check-only guard" "no CHECK_ONLY guard"
    fi
else
    bad "positive control: the script has mutating calls to guard" "none found"
fi

echo
echo "lingering is requested with elevation, and waited for:"
# THE RUNNER IS A SERVICE, NOT A LOGIN SESSION, so /run/user/<uid> does not exist and
# rootless podman has nowhere to keep its state. enable-linger is what makes systemd
# create and maintain that directory for a user with no session -- and it needs root,
# so calling it unprivileged fails silently and the gap surfaces two checks later as
# something that reads like a podman problem. Observed on the first ephemeral run:
#   runner-deps: MISSING /run/user/1001 does not exist
if grep -qE '^[^#]*sudo[^#]*loginctl[^#]*enable-linger' "$SCRIPT"; then
    ok "enable-linger is called with elevation"
else
    bad "enable-linger is called with elevation" "loginctl enable-linger is called unprivileged"
fi
# logind creates the directory asynchronously; checking immediately races it.
if grep -qE 'for .*in .*seq|while .*\[ .*-lt |sleep ' "$SCRIPT"; then
    ok "it waits for the runtime directory to appear"
else
    bad "it waits for the runtime directory to appear" "no bounded wait after enable-linger"
fi

echo
echo "the log reads in the order things happened:"
# note() on stdout and gap() on stderr interleave unpredictably in a CI log: the first
# run printed "podman rootless ok" AFTER "this machine cannot run the suites", which
# reads as a contradiction and sends the reader to the wrong half of the script.
_nstream="$(grep -cE '^note\(\) *\{.*>&2' "$SCRIPT")"
if [ "${_nstream:-0}" -ge 1 ]; then
    ok "progress and gaps share one stream"
else
    bad "progress and gaps share one stream" "note() writes to stdout while gap() writes to stderr"
fi

echo
echo "rootless networking has a provider the installed podman will actually use:"
# podman 5 defaults rootless networking to pasta, not slirp4netns, and falls back to
# nothing: with pasta absent it aborts the container with
#   Error: could not find pasta, the network namespace can't be configured
# which testenv-batch reports as rc=2 and the gate attributes as a harness fault --
# correct attribution, but the run is still lost. slirp4netns alone is not enough on
# a distro shipping podman 5; the binary is called pasta and the package is passt.
if grep -qE '^PKGS=\(|^ +' "$SCRIPT" && grep -qE '(^|[^a-z-])passt([^a-z-]|$)' "$SCRIPT"; then
    ok "the package list provides pasta"
else
    bad "the package list provides pasta" "no passt in PKGS; podman 5 rootless cannot configure a netns"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
