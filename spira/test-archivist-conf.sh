#!/usr/bin/env bash
#
# test-archivist-conf.sh — archivist's conf keys, static (UC-35).
#
#   ./test-archivist-conf.sh
#
# Split out of test-archivist.sh (a T2 behavioural suite) because these checks exercise no
# code path — they read conf.sh and assert on the key list and defaults, which is a T0
# lint concern, not a unit test.
#
# tier: T0
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "no [$3] in [$2]" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "found [$3] in [$2]" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"
mkdir -p "$T/home" "$T/run"

# ==========================================================================================
echo
echo "SPIRA_ARCHIVIST_AT is not in conf.sh's key list; SPIRA_ARCHIVIST_EVERY is, and defaults to 40"
# ==========================================================================================
out="$(env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" SPIRA_RUN="$T/run" \
    bash -c '. "'"$HERE"'/conf.sh" && echo "$SPIRA_CONF_KEYS"' 2>/dev/null)"
hasnt "SPIRA_ARCHIVIST_AT is absent from the key list" "$out" "SPIRA_ARCHIVIST_AT"
has "SPIRA_ARCHIVIST_EVERY is in the key list" "$out" "SPIRA_ARCHIVIST_EVERY"

val="$(env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" SPIRA_RUN="$T/run" \
    bash -c '. "'"$HERE"'/conf.sh" && echo "$SPIRA_ARCHIVIST_EVERY"' 2>/dev/null)"
is "default SPIRA_ARCHIVIST_EVERY" "40" "$val"

# ==========================================================================================
echo
echo "SPIRA_ARCHIVIST_PER_PASS is in the key list and defaults to 1"
# ==========================================================================================
out="$(env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" SPIRA_RUN="$T/run" \
    bash -c '. "'"$HERE"'/conf.sh" && echo "$SPIRA_CONF_KEYS"' 2>/dev/null)"
has "SPIRA_ARCHIVIST_PER_PASS is in the key list" "$out" "SPIRA_ARCHIVIST_PER_PASS"
val="$(env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" SPIRA_RUN="$T/run" \
    bash -c '. "'"$HERE"'/conf.sh" && echo "$SPIRA_ARCHIVIST_PER_PASS"' 2>/dev/null)"
is "default SPIRA_ARCHIVIST_PER_PASS" "1" "$val"

# ==========================================================================================
echo
echo "SPIRA_ARCHIVIST_TIMEOUT_RETRIES is in the key list and defaults to 3"
# ==========================================================================================
out="$(env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" SPIRA_RUN="$T/run" \
    bash -c '. "'"$HERE"'/conf.sh" && echo "$SPIRA_CONF_KEYS"' 2>/dev/null)"
has "SPIRA_ARCHIVIST_TIMEOUT_RETRIES is in the key list" "$out" "SPIRA_ARCHIVIST_TIMEOUT_RETRIES"
val="$(env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" SPIRA_RUN="$T/run" \
    bash -c '. "'"$HERE"'/conf.sh" && echo "$SPIRA_ARCHIVIST_TIMEOUT_RETRIES"' 2>/dev/null)"
is "default SPIRA_ARCHIVIST_TIMEOUT_RETRIES" "3" "$val"

echo
tl_summary
