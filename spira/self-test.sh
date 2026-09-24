#!/usr/bin/env bash
#
# self-test.sh <release-dir> — the release's own self-test: every binary the
# MANIFEST names is present, executable, and hashes to what MANIFEST recorded
# at build time. Catches a release corrupted or hand-edited between `make
# install` writing it and pre-activate.sh checking it.
set -uo pipefail

REL="${1:?usage: self-test.sh <release-dir>}"
MANIFEST="$REL/MANIFEST"

[ -f "$MANIFEST" ] || { printf 'self-test: no MANIFEST in %s\n' "$REL" >&2; exit 1; }

fail=0
while IFS= read -r line; do
    case "$line" in
        bin/*)
            name="${line%% *}"
            want="${line#* }"
            path="$REL/$name"
            if [ ! -x "$path" ]; then
                printf 'self-test: FAIL %s: missing or not executable\n' "$name" >&2
                fail=1
                continue
            fi
            got="$(sha256sum "$path" | awk '{print $1}')"
            if [ "$got" != "$want" ]; then
                printf 'self-test: FAIL %s: sha256 %s, MANIFEST says %s\n' "$name" "$got" "$want" >&2
                fail=1
            fi
            ;;
    esac
done < "$MANIFEST"

exit "$fail"
