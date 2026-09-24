#!/usr/bin/env bash
#
# test-testenv-registry.sh — the image is acquired, not necessarily built.
#
# WHY THIS EXISTS. The test image is 1.8 GB and its build downloads a Go
# toolchain, compiles bd from source and installs a Rust toolchain. A machine
# that keeps it between runs pays that once; a machine created for one run and
# destroyed after pays it every time. So a configured registry is consulted
# before building.
#
# THE PROPERTY THAT MUST NOT BE LOST. The tag is the hash of the build closure —
# Containerfile, bd pin, and conf.sh's dependency manifest. Acquiring an image by
# pulling that tag is exactly as safe as building it, and for the same reason:
# a change to any input produces a different tag, so a stale image is
# unreachable rather than merely unlikely. Pushing under a floating tag would
# destroy that, which is why publishing a floating tag is asserted against here
# rather than left to discipline.
#
# WHAT IS TESTED
#   1. No registry configured: nothing is pulled. The existing behaviour of every
#      machine that has not opted in must be bit-identical.
#   2. Registry configured, image absent locally: a pull is attempted, under the
#      content-addressed tag.
#   3. Pull hits: no build runs. This is the whole point.
#   4. Pull misses: the build runs anyway. A registry that is empty, unreachable
#      or unauthenticated must be slow, never fatal -- the first run after a
#      Containerfile change necessarily misses.
#   5. Whatever happened, callers are handed the same local ref as before.
#   6. publish pushes the content-addressed tag and no floating one.
#
# FIXTURES. podman is stubbed on PATH and driven by environment variables, so no
# image is pulled, built or pushed by this suite. The stub records its own argv,
# which is what every assertion here reads.
#
# covers: spira/testenv.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
saw()    { if grep -q -- "$2" "$3" 2>/dev/null; then ok "$1"; else bad "$1" "no [$2] in podman calls"; fi; }
notsaw() { if grep -q -- "$2" "$3" 2>/dev/null; then bad "$1" "podman was called with [$2]"; else ok "$1"; fi; }

echo "test-testenv-registry.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
FIXTURE="$TMP/spira"
mkdir -p "$FIXTURE/testenv" "$TMP/bin"
cp "$HERE/testenv.sh"            "$FIXTURE/testenv.sh"
cp "$HERE/conf.sh"               "$FIXTURE/conf.sh"
cp "$HERE/testenv/Containerfile" "$FIXTURE/testenv/Containerfile"
PIN="$TMP/bd-pin"; printf 'BD_PIN_MIGRATIONS=42\n' > "$PIN"

# podman stub. STUB_LOCAL=1 means the tag is already present; STUB_PULL=0 means
# the pull fails, which is what an empty or unreachable registry looks like.
cat > "$TMP/bin/podman" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PODMAN_LOG"
case "$1" in
    image)  [ "${STUB_LOCAL:-0}" = "1" ] && exit 0; exit 1 ;;
    pull)   [ "${STUB_PULL:-0}"  = "1" ] && exit 0; exit 1 ;;
    build|tag|push) exit 0 ;;
esac
exit 0
STUB
chmod +x "$TMP/bin/podman"

# run <registry> <local?> <pull?> <subcommand...> -> stdout of testenv.sh; log in $LOG
#
# LOG IS FIXED HERE, NOT ASSIGNED INSIDE run(). Every caller invokes run in a
# command substitution, which is a subshell: an assignment made there is gone by
# the time the assertions read it, and grep against an empty path matches nothing
# and reads as "the call never happened".
LOG="$TMP/podman.log"
run() {
    local reg="$1" loc="$2" pul="$3"; shift 3
    : > "$LOG"
    PATH="$TMP/bin:$PATH" PODMAN_LOG="$LOG" STUB_LOCAL="$loc" STUB_PULL="$pul" \
    SPIRA_BD_PIN="$PIN" SPIRA_TESTENV_REGISTRY="$reg" \
        bash "$FIXTURE/testenv.sh" "$@" 2>/dev/null
}

TAG="$(SPIRA_BD_PIN="$PIN" bash "$FIXTURE/testenv.sh" tag 2>/dev/null)"
echo
if [ -n "$TAG" ]; then ok "positive control: the closure hash resolves ($TAG)"
else bad "positive control: the closure hash resolves" "empty"; fi

echo
echo "1. no registry configured — nothing is pulled:"
out="$(run "" 0 0 image)"
notsaw "no pull is attempted"  "pull" "$LOG"
saw    "the image is built"    "build" "$LOG"

echo
echo "2 & 3. a configured registry is consulted, and a hit skips the build:"
out="$(run "example.invalid/spira" 0 1 image)"
saw    "a pull is attempted"                 "pull" "$LOG"
saw    "the pull names the closure hash"     "$TAG" "$LOG"
notsaw "a hit does not build"                "build" "$LOG"

echo
echo "4. a miss falls back to building, and is not fatal:"
out="$(run "example.invalid/spira" 0 0 image)"; rc=$?
saw "a pull was attempted" "pull"  "$LOG"
saw "the build ran"        "build" "$LOG"
[ "$rc" -eq 0 ] && ok "a registry miss is not an error" \
                || bad "a registry miss is not an error" "exited $rc"

echo
echo "5. callers are handed the same local ref either way:"
a="$(run ""                      1 0 image)"
b="$(run "example.invalid/spira" 1 0 image)"
if [ -n "$a" ] && [ "$a" = "$b" ]; then ok "the ref does not depend on how it was acquired ($a)"
else bad "the ref does not depend on how it was acquired" "[$a] vs [$b]"; fi
case "$a" in
    localhost/*) ok "the ref stays local, so nothing downstream changes" ;;
    *)           bad "the ref stays local, so nothing downstream changes" "got [$a]" ;;
esac

echo
echo "6. publish pushes the closure hash and nothing floating:"
out="$(run "example.invalid/spira" 1 0 publish)"
saw    "it pushes"                      "push"   "$LOG"
saw    "it pushes the closure hash"     "$TAG"   "$LOG"
notsaw "it does not push :latest"       ":latest" "$LOG"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
