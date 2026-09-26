#!/usr/bin/env bash
# tsd-timings-json.sh — best-effort per-suite p50 wall time, as JSON {"test-x.sh": ms, ...},
# for the coverage matrix's TIER HONESTY column.
#
# Sourced from run/tsd/ (sp-sbc6o) via spira/tsd-query.sh's `suite-p50-json` subcommand, when
# both exist. Neither is a hard dependency of this repository's own tree yet, and a branch
# that has not built the time-series layer must still be able to regenerate a matrix — so
# any failure here (script absent, subcommand unrecognised, no data) degrades to `{}` rather
# than failing the caller. `{}` means every suite's runtime is unknown, not zero.
#
# tier: T0
# covers: spira/tsd-timings-json.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

if [ -x "$HERE/tsd-query.sh" ]; then
    if out="$("$HERE/tsd-query.sh" suite-p50-json 2>/dev/null)" && [ -n "$out" ]; then
        printf '%s\n' "$out"
        exit 0
    fi
fi
printf '{}\n'
