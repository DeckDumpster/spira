#!/usr/bin/env bash
#
# build-aerc.sh — place aerc at <vendor-bin-dir>/aerc for the release tarball.
#
# Usage: build-aerc.sh <vendor-bin-dir>
#
# The source is the aerc in the harness flake's spira-env, pinned by flake.lock and served
# from the nix binary cache. Nothing is cloned from a third-party git host at release time:
# a release must not depend on one being reachable at the minute it runs.
#
# EXIT
#   0  <vendor-bin-dir>/aerc exists and reports exactly AERC_VERSION
#   1  any failure, always loud: no nix, the nix build still failing after every attempt
#      (the message names the source), or a binary that does not report the pinned version.
#      A swallowed failure ships a release with no aerc at all.
#
# ENV
#   NIX_BIN             nix to use, tried before the standard profile locations and PATH
#   BUILD_AERC_NIX_CANDIDATES  space-separated nix paths replacing the standard locations
#   BUILD_AERC_ATTEMPTS  nix build attempts before giving up (default 4)
#   BUILD_AERC_BACKOFF   seconds before the first retry, doubling each time (default 5)
set -uo pipefail

AERC_VERSION="0.22.0"
ATTEMPTS="${BUILD_AERC_ATTEMPTS:-4}"
BACKOFF="${BUILD_AERC_BACKOFF:-5}"

VENDOR_BIN="${1:?usage: build-aerc.sh <vendor-bin-dir>}"

NIX=""
searched=""
for cand in "${NIX_BIN:-}" ${BUILD_AERC_NIX_CANDIDATES-/nix/var/nix/profiles/default/bin/nix "$HOME/.nix-profile/bin/nix" /run/current-system/sw/bin/nix}; do
    [ -n "$cand" ] || continue
    searched="$searched $cand"
    [ -x "$cand" ] && { NIX="$cand"; break; }
done
if [ -z "$NIX" ]; then
    NIX="$(command -v nix 2>/dev/null)" || { printf 'build-aerc.sh: no nix to provide aerc (looked at:%s, and nix on PATH=%s)\n' "$searched" "$PATH" >&2; exit 1; }
fi
flake_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source_name="nix flake path:$flake_dir#spira-env (aerc, nix binary cache)"
printf 'build-aerc.sh: taking aerc %s from %s\n' "$AERC_VERSION" "$source_name" >&2

env_dir=""
attempt=1
delay="$BACKOFF"
while :; do
    # batch-job: the release build realises spira-env once per release; bounded at 900 s.
    if env_dir="$(timeout 900 "$NIX" --extra-experimental-features 'nix-command flakes' build "path:$flake_dir#spira-env" --no-link --print-out-paths)" \
        && [ -x "$env_dir/bin/aerc" ]; then
        break
    fi
    if [ "$attempt" -ge "$ATTEMPTS" ]; then
        printf 'build-aerc.sh: cannot obtain aerc %s from %s after %s attempts\n' "$AERC_VERSION" "$source_name" "$attempt" >&2
        exit 1
    fi
    printf 'build-aerc.sh: fetch from %s failed (attempt %s/%s); retrying in %ss\n' "$source_name" "$attempt" "$ATTEMPTS" "$delay" >&2
    sleep "$delay"
    delay=$((delay * 2))
    attempt=$((attempt + 1))
done

mkdir -p "$VENDOR_BIN" && install -m 755 "$env_dir/bin/aerc" "$VENDOR_BIN/aerc" \
    || { printf 'build-aerc.sh: cannot place aerc at %s\n' "$VENDOR_BIN/aerc" >&2; exit 1; }

ver="$(timeout 5 "$VENDOR_BIN/aerc" -v 2>/dev/null)" || ver=""
case "$ver" in
    "aerc $AERC_VERSION "*) ;;
    *)
        printf 'build-aerc.sh: built %s does not report version %s (got: %s)\n' "$VENDOR_BIN/aerc" "$AERC_VERSION" "$ver" >&2
        exit 1
        ;;
esac
printf 'build-aerc.sh: built aerc %s at %s\n' "$AERC_VERSION" "$VENDOR_BIN/aerc" >&2
