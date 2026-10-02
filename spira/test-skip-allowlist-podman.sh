#!/usr/bin/env bash
#
# test-skip-allowlist-podman.sh — a suite that skips when podman is absent must have a
# skip-allowlist.tsv row for that requirement, or testenv counts the skip red inside its
# container (which never carries podman).
#
# tier: T2
# covers: spira/test-*.sh spira/skip-allowlist.tsv
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-skip-allowlist-podman.sh"

podman_skippers() {
    local dir="$1" f
    for f in "$dir"/test-*.sh; do
        [ "$(basename "$f")" = "test-skip-allowlist-podman.sh" ] && continue
        grep -Eq '(command -v podman|for [_a-z]+ in [^;]*\bpodman\b)' "$f" || continue
        grep -Eq '(\bskip "|exit 77|SKIP)' "$f" && basename "$f"
    done
}

missing() {
    local dir="$1" allow="$2" s
    for s in $(podman_skippers "$dir"); do
        awk -F'\t' -v s="$s" '$1 == s && $2 ~ /^skip:podman/ { f = 1 } END { exit !f }' "$allow" || echo "$s"
    done
}

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
printf '#!/bin/bash\ncommand -v podman >/dev/null || skip "podman gone"\n' >"$T/test-planted.sh"
: >"$T/allow.tsv"
[ "$(missing "$T" "$T/allow.tsv")" = "test-planted.sh" ] \
    && ok "an unallowlisted podman skipper is reported" \
    || bad "an unallowlisted podman skipper is reported" "matcher found nothing"
printf 'test-planted.sh\tskip:podman_gone\twhy\n' >"$T/allow.tsv"
[ -z "$(missing "$T" "$T/allow.tsv")" ] \
    && ok "an allowlisted podman skipper passes" \
    || bad "an allowlisted podman skipper passes" "still reported"

[ -n "$(podman_skippers "$HERE")" ] \
    && ok "the real tree has podman skippers to check" \
    || bad "the real tree has podman skippers to check" "none found"
M="$(missing "$HERE" "$HERE/skip-allowlist.tsv")"
[ -z "$M" ] && ok "every podman-skipping suite is in skip-allowlist.tsv" \
    || bad "every podman-skipping suite is in skip-allowlist.tsv" "missing: $M"

tl_summary
