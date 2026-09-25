#!/usr/bin/env bash
#
# broker-allowlist-lint.sh — the broker's shipped env vars are declared where conf.sh's
# allowlist and build.sh's manifest can see them.
#
#   broker-allowlist-lint.sh                scan the shipped conf.sh/build.sh; exit 1 naming
#                                            any missing name
#   broker-allowlist-lint.sh --scan-conf F   scan one file as a stand-in for conf.sh
#   broker-allowlist-lint.sh --scan-build F  scan one file as a stand-in for build.sh
#   broker-allowlist-lint.sh --scan-shim F   scan one file as a stand-in for broker.sh
#
# WHY THIS IS A LINT, NOT A CARGO-GATED SUITE. test-broker.sh used to carry these as grep
# assertions behind "if cargo is missing, skip the whole suite" — so a checkout with no Rust
# toolchain never ran them, even though none of the five checks below touches the broker
# binary at all. Splitting them out means they run everywhere test-*.sh runs, cargo or not.
#
# THE NAMES. conf.sh's env-var allowlist must carry every variable the broker reads:
# SPIRA_BROKER_BIN, SPIRA_BROKER_GH_CONFIG_DIR, SPIRA_BROKER_GH_TOKEN, SPIRA_GH,
# SPIRA_GH_APP_CONFIG. build.sh must mention broker (it is what builds the release binary).
# broker.sh must exec the binary rather than reimplement it, and must source conf.sh rather
# than read the environment raw.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

CONF_NAMES=(SPIRA_BROKER_BIN SPIRA_BROKER_GH_CONFIG_DIR SPIRA_BROKER_GH_TOKEN SPIRA_GH SPIRA_GH_APP_CONFIG)

scan_conf() {   # scan_conf <file> -> missing names, one per line; exit 0 either way
    local f="$1" name
    for name in "${CONF_NAMES[@]}"; do
        grep -q "$name" "$f" 2>/dev/null || printf '%s\n' "$name"
    done
}

scan_build() {  # scan_build <file> -> "broker" if build.sh never mentions it
    grep -q 'broker' "$1" 2>/dev/null || printf 'broker\n'
}

scan_shim() {   # scan_shim <file> -> missing shim properties, one per line
    local f="$1" content
    content="$(cat "$f" 2>/dev/null)"
    [[ "$content" == *"exec"* ]]    || printf 'execs the broker binary\n'
    [[ "$content" == *"conf.sh"* ]] || printf 'sources conf.sh\n'
    [[ "$content" == *"case"* ]]    && printf 'contains no logic (found a case statement)\n'
}

case "${1:-}" in
    --scan-conf)  scan_conf "${2:?--scan-conf needs a file}"; exit 0 ;;
    --scan-build) scan_build "${2:?--scan-build needs a file}"; exit 0 ;;
    --scan-shim)  scan_shim "${2:?--scan-shim needs a file}"; exit 0 ;;
esac

bad=0
conf_missing="$(scan_conf "$HERE/conf.sh")"
if [ -n "$conf_missing" ]; then
    bad=1
    while IFS= read -r name; do
        printf 'broker-allowlist-lint: conf.sh: missing %s\n' "$name"
    done <<< "$conf_missing"
fi

build_missing="$(scan_build "$HERE/build.sh")"
if [ -n "$build_missing" ]; then
    bad=1
    printf 'broker-allowlist-lint: build.sh: does not mention broker\n'
fi

if [ -f "$HERE/broker.sh" ] && [ -x "$HERE/broker.sh" ]; then
    shim_missing="$(scan_shim "$HERE/broker.sh")"
    if [ -n "$shim_missing" ]; then
        bad=1
        while IFS= read -r reason; do
            printf 'broker-allowlist-lint: broker.sh: %s\n' "$reason"
        done <<< "$shim_missing"
    fi
else
    bad=1
    printf 'broker-allowlist-lint: broker.sh is not present or not executable\n'
fi

if [ "$bad" = 0 ]; then
    printf 'broker-allowlist-lint: clean\n'
    exit 0
fi
exit 1
