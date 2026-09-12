#!/usr/bin/env bash
#
# doctor-check.sh — verify doctor.sh's declared programs are in PATH or waived.
#
# Usage: doctor-check.sh <path/to/doctor.sh> [<path/to/waivers>]
#
# doctor.sh contains two `for b in` loops:
#   1. FATAL: `for b in bd git python3 flock; do`  — any absent program fails
#   2. WARN:  `for b in dolt gh "${SPIRA_AGENT:-claude}" tmux cargo node; do`
#                                                   — absent programs must be waived
#
# The program lists are read from doctor.sh directly, not hardcoded here. That is
# the whole point: a program added to doctor.sh's lists fails the image build until
# the image carries it. A second list that must agree with the source is cause B.
#
# WAIVER FILE FORMAT. One program per non-comment, non-blank line:
#   <name>  <reason text>
# An empty reason is refused: an omission must be a written decision, not a silence.
#
# covers: spira/testenv/Containerfile spira/doctor.sh
set -uo pipefail

DOCTOR="${1:?usage: doctor-check.sh <doctor.sh> [<waivers>]}"
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

# ── Parse doctor.sh's two program loops ──────────────────────────────────────
# doctor.sh contains lines of the form:
#   for b in bd git python3 flock; do
# and:
#   for b in dolt gh "${SPIRA_AGENT:-claude}" tmux cargo node; do
#
# We extract the Nth such line. This is a structural expectation about doctor.sh's
# format: two consecutive `for b in` loops, FATAL first, WARN second.

_extract_for_loop() {
    awk -v target="$1" '
        /^for b in .*; do$/ {
            count++
            if (count == target) {
                sub(/^for b in /, "")
                sub(/; do$/, "")
                print
                exit
            }
        }
    ' "$DOCTOR"
}

fatal_line="$(_extract_for_loop 1)"
warn_line="$(_extract_for_loop 2)"

if [ -z "$fatal_line" ] || [ -z "$warn_line" ]; then
    printf 'doctor-check: could not parse program loops from %s\n' "$DOCTOR" >&2
    printf 'doctor-check: expected two lines matching ^for b in .*; do$\n' >&2
    exit 1
fi

# Expand ${SPIRA_AGENT:-claude} in the warn line. The sed replaces the literal
# shell-parameter form with the resolved value, then strips the enclosing quotes.
# The value is SPIRA_AGENT when set, or "claude" when not.
_agent="${SPIRA_AGENT:-claude}"
warn_line_expanded="$(printf '%s' "$warn_line" \
    | sed 's/"\${SPIRA_AGENT:-[^}]*}"/'"$_agent"'/g' \
    | tr -d '"')"

read -ra fatal_progs <<< "$fatal_line"
read -ra warn_progs  <<< "$warn_line_expanded"

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
