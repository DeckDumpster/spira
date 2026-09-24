#!/usr/bin/env bash
#
# doctor-check.sh — verify the dependency manifest's runtime/optional/operator programs
# are in PATH or waived.
#
# Usage: doctor-check.sh <path/to/conf.sh> [<path/to/waivers>]
#
# THE MANIFEST IS conf.sh's deps.toml (loaded via spira_deps_list/spira_bin_tier) — not a
# copy of it. Reading conf.sh directly means a program added to deps.toml fails the image
# build until the image carries it, with no second list that must agree with the first.
#
#   runtime            FATAL: any absence fails the image build
#   optional, operator  WARN: present or waived, with a written reason
#   dev                 not checked here — the image is what tests need to RUN against, and
#                       dev-tier programs (bd-embedded, podman) are what tests need to EXIST
#
# WAIVER FILE FORMAT. One program per non-comment, non-blank line:
#   <name>  <reason text>
# An empty reason is refused: an omission must be a written decision, not a silence.
#
# covers: spira/testenv/Containerfile spira/conf.sh
set -uo pipefail

CONF="${1:?usage: doctor-check.sh <conf.sh> [<waivers>]}"
WAIVERS="${2:-}"

fail_count=0
ok_count=0
waived_count=0

ok()     { ok_count=$((ok_count+1));     printf '  ok      %s\n' "$1"; }
bad()    { fail_count=$((fail_count+1)); printf '  FAIL    %s\n' "$1"; }
waived() { waived_count=$((waived_count+1)); printf '  waived  %s — %s\n' "$1" "$2"; }

# ── Read waivers ──────────────────────────────────────────────────────────────
declare -A WAIVE_REASON
if [ -n "$WAIVERS" ] && [ -f "$WAIVERS" ]; then
    while IFS= read -r line; do
        # Skip blank lines and comments.
        [[ "$line" =~ ^[[:space:]]*($|#) ]] && continue
        # First whitespace-delimited token is the program name; the rest is the reason.
        prog="${line%%[[:space:]]*}"
        rest="${line#"$prog"}"
        reason="${rest#"${rest%%[![:space:]]*}"}"   # left-trim whitespace
        if [ -z "${reason:-}" ]; then
            printf 'doctor-check: waiver for "%s" has no reason — refusing to waive\n' \
                "$prog" >&2
            exit 1
        fi
        WAIVE_REASON["$prog"]="$reason"
    done < "$WAIVERS"
fi

# ── Read the manifest from conf.sh, tiered ────────────────────────────────────
# An explicit, minimal environment: a real spira.conf on this box (or in this image build)
# must not decide what the image is checked against (law-gates-run-in-a-clean-environment).
manifest="$(env -i HOME="${HOME:-/root}" PATH="$PATH" SPIRA_CONF=/nonexistent bash -c '
    . '"$(printf '%q' "$CONF")"' 2>/dev/null
    for b in $(spira_deps_list); do
        printf "%s %s\n" "$b" "$(spira_bin_tier "$b")"
    done
')"

if [ -z "$manifest" ]; then
    printf 'doctor-check: could not read a manifest from %s (deps.toml did not load)\n' "$CONF" >&2
    exit 1
fi

fatal_progs=(); warn_progs=()
while read -r prog tier; do
    [ -n "$prog" ] || continue
    case "$tier" in
        runtime)            fatal_progs+=("$prog") ;;
        optional|operator)  warn_progs+=("$prog") ;;
        dev)                ;;
    esac
done <<< "$manifest"

# ── Check FATAL programs ──────────────────────────────────────────────────────
printf '\nFATAL programs — any absence fails the image build:\n'
for prog in "${fatal_progs[@]}"; do
    if command -v "$prog" >/dev/null 2>&1; then
        ok "$prog ($(command -v "$prog"))"
    else
        bad "$prog — not found on PATH"
    fi
done

# ── Check WARN programs ───────────────────────────────────────────────────────
printf '\nWARN programs — must be present or explicitly waived:\n'
for prog in "${warn_progs[@]}"; do
    if command -v "$prog" >/dev/null 2>&1; then
        ok "$prog ($(command -v "$prog"))"
    elif [ -n "${WAIVE_REASON[$prog]:-}" ]; then
        waived "$prog" "${WAIVE_REASON[$prog]}"
    else
        bad "$prog — not found on PATH and not waived (add to waivers with a reason)"
    fi
done

# ── Summary ───────────────────────────────────────────────────────────────────
printf '\n%d ok, %d waived, %d FAIL\n' "$ok_count" "$waived_count" "$fail_count"

if [ "$fail_count" -gt 0 ]; then
    printf 'doctor-check: image build FAILED — install missing programs or waive with a reason\n' >&2
    exit 1
fi
printf 'doctor-check: all programs present or waived\n'
exit 0
