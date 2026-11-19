#!/usr/bin/env bash
#
# test-no-bd-callers.sh — the live shell callers reach the store only through spira-lc.
#
# The matcher is shown a planted offender first; a silent scan is believed only after that.
#
# tier: T1
# covers: spira/lib.sh spira/pilgrimage.sh spira/confine.sh spira/reflect.sh spira/law-synth.sh spira/first-run.sh spira/verify-landed.sh spira/seed.sh spira/ready.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

# A command word: first word of a line or of a pipeline/list/substitution stage, after any
# VAR=value prefixes. Comment lines and words inside quoted messages are not command words.
offenders() {   # offenders <file>: prints each offending line
    grep -nE '(^|[|;&(`]|\$\()[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*(bdq|bdjson|bd[[:space:]]+-C)([[:space:]]|$)' "$1" \
        | grep -vE '^[0-9]+:[[:space:]]*#'
}

PLANT="$(mktemp)"
trap 'rm -f "$PLANT"' EXIT
printf '%s\n' 'bdq note x y' 'BD_TIMEOUT=5 bdq comments x' 'x="$(bdjson show y)"' 'a | bd -C /db list' > "$PLANT"
is "the matcher finds a planted bdq, a prefixed bdq, a bdjson and a bd -C" 4 "$(offenders "$PLANT" | wc -l)"
printf '%s\n' '# bdq note x y' 'echo "run: bd -C /db init"' 'spira-lc content note x --stdin' > "$PLANT"
is "the matcher leaves comments, quoted messages and the door alone" 0 "$(offenders "$PLANT" | wc -l)"

for f in lib pilgrimage confine reflect law-synth first-run verify-landed seed ready; do
    [ -f "$HERE/$f.sh" ] || { bad "$f.sh exists" "missing"; continue; }
    is "$f.sh has no bdq, bdjson or bd -C command word" "" "$(offenders "$HERE/$f.sh")"
done

tl_summary
