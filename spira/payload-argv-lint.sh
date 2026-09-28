#!/usr/bin/env bash
#
# payload-argv-lint.sh — refuse a JSON payload handed to python3/jq/awk through argv or an
# environment variable (law-payloads-go-on-stdin).
#
#   payload-argv-lint.sh              scan tracked spira/*.sh; exit 1 naming each offender
#   payload-argv-lint.sh --scan FILE  scan one file; print line:text per hit; exit 0 either way
#
# THE PROPERTY. A store-sized JSON payload (a bd list/ready/show/children result — anything
# that scales with the number of beads) crosses MAX_ARG_STRLEN (128 KiB, per argv element AND
# per environment string) silently: the exec dies "Argument list too long", the caller reads
# empty output, and empty reads as "nothing to do" rather than as a fault. Five outages in
# seventeen days, the last an hour of 572 idle summons against 142 ready beads (sp-o4trx).
# Every occurrence was fixed only at its own call site; nothing scanned for the shape until
# this fence existed.
#
# WHAT IS MATCHED, two shapes, both seen in this tree before this bead:
#   ARGV     python3 -c '            <- OPENS the inline script: python3/jq/awk + a quote
#             ...
#            ' "$some_json"          <- CLOSES it: text after the closing quote carries a
#                                        "$var" token whose name contains "json"
#   ENV      SOME_JSON="$payload" python3 -c '...'
#            a JSON-named variable assigned a plain value (a literal or ${...} expansions,
#            never a nested command substitution) immediately before python3/jq/awk
#
# THE ARGV CHECK TRACKS WHETHER AN INLINE SCRIPT IS STILL OPEN, one quote character at a
# time, across lines — a bare "line starts with a quote and mentions a *_json var" check
# also fires on an ordinary multi-line `printf` argument list, which has nothing to do with
# python3/jq/awk at all. Tracking the opener means the closing-quote line is only trusted
# once its own opener named one of the three commands. This relies on the same invariant
# the shell scripts here already keep for exactly this reason: a single-quoted inline
# script may never itself contain an apostrophe (bash cannot express one inside '...'
# either), so counting quote characters textually is exact, not approximate.
#
# NEITHER SHAPE MATCHES `printf '%s' "$x_json" | python3 -c '...'` — piping a payload to
# stdin is the FIX, not the defect, even though the line still names both python3 and a
# *_json variable; that line OPENS a script, it does not close one carrying trailing args.
#
# This is a LINE-LEVEL heuristic, not a parser — every instance of the defect in this tree
# put the offending tokens on one line, closing-quote-then-args or var-then-invocation. A
# positive control (below, and in the suite) means the day it stops firing at all is caught,
# not the day it stops catching every conceivable shape.
#
# A NAMED VARIABLE IS NOT THE DEFECT BY ITSELF. `started_csv`, `resume_csv` and similar short
# id lists are bounded by the number of epics/branches in flight, never by store size — this
# fence flags the token pattern (a variable whose name contains "json"), not every argv/env
# variable, because a *_json name is this codebase's own convention for "this holds a bd
# query result" (every unbounded payload fixed under sp-o4trx was named this way). A
# legitimate exception belongs in payload-argv-lint-allow, not a broader regex here.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
ALLOW="$HERE/payload-argv-lint-allow"

# The awk program lives in a quoted heredoc so its literal quote characters need no bash
# escaping at all. STATE: in_script/open_q track an inline script opened on an earlier
# line and not yet closed — reset per file (scan() runs awk once per file argument).
AWK_PROG="$(cat <<'AWK_EOF'
BEGIN { in_script = 0 }
{
    stripped = $0
    sub(/^[ \t]*/, "", stripped)
    is_comment = (stripped ~ /^#/)
    low = tolower($0)

    # ENV shape is single-line and independent of script-nesting state: a JSON-named
    # variable assigned a PLAIN value (a quoted literal or ${...} expansions only, never a
    # nested command substitution) immediately before python3/jq/awk. Excluding "$(" from
    # the value is what keeps `ready_json="$(SPIRA_SCOPE_LABEL=... python3 -c ...)"` — an
    # outer variable simply CAPTURING a subshell's output — from matching: SPIRA_SCOPE_LABEL
    # there is not "*json*" and ready_json is not read as an environment variable BY python3.
    if (!is_comment && low ~ /[a-z_][a-z0-9_]*json[a-z0-9_]*="(\$\{[^}]*\}|[^"$])*".*(python3|jq|awk)/) {
        printf "%d:%s\n", NR, $0
    }

    if (in_script) {
        idx = index($0, open_q)
        if (idx > 0) {
            after = tolower(substr($0, idx + 1))
            if (!is_comment && after ~ /"\$[a-z_][a-z0-9_]*json[a-z0-9_]*"/) {
                printf "%d:%s\n", NR, $0
            }
            in_script = 0
        }
        next
    }

    if (!is_comment && match(low, /(python3[ \t]+-c|jq[ \t]|awk)[ \t]+['"]/)) {
        qpos = RSTART + RLENGTH - 1
        open_q = substr(low, qpos, 1)
        rest = substr($0, qpos + 1)
        n = gsub(open_q, open_q, rest)
        if (n % 2 == 0) {
            in_script = 1
        } else if (tolower(rest) ~ /"\$[a-z_][a-z0-9_]*json[a-z0-9_]*"/) {
            # Opens and closes on the same line — check the trailing text too.
            printf "%d:%s\n", NR, $0
        }
    }
}
AWK_EOF
)"

# scan <file> -> line:text per hit, one pass, comment lines excluded; exit 0 either way.
scan() {
    awk "$AWK_PROG" "$1"
}

case "${1:-}" in
--scan) scan "${2:?--scan needs a file}"; exit 0 ;;
esac

git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || {
    printf 'payload-argv-lint: %s is not a git repository — nothing to scan\n' "$ROOT" >&2; exit 3; }

mapfile -t files < <(git -C "$ROOT" ls-files -- 'spira/*.sh' 2>/dev/null)
[ "${#files[@]}" -gt 0 ] || {
    printf 'payload-argv-lint: no spira/*.sh tracked — refusing to report clean\n' >&2; exit 3; }

# The allow-list, one extended regex per line, matched against "file:line: text" — the same
# shape inventory.sh's own deny-list uses, so an exemption reads the same way in both places.
mapfile -t allow_pats < <(grep -vE '^[[:space:]]*(#|$)' "$ALLOW" 2>/dev/null)

bad=0
_offenders=()
for f in "${files[@]}"; do
    case "$f" in
        # THIS FENCE'S OWN TEST — the planted offender is content under test, not a live
        # invocation, exactly like bd-stdin-lint.sh's own exclusion of its own suite.
        */test-payload-argv-lint.sh|test-payload-argv-lint.sh) continue ;;
        # THIS FENCE'S OWN SOURCE — the docstring above quotes the offending shapes.
        */payload-argv-lint.sh|payload-argv-lint.sh) continue ;;
    esac
    [ -f "$ROOT/$f" ] || continue
    hits="$(scan "$ROOT/$f")"
    [ -n "$hits" ] || continue
    while IFS=: read -r ln text; do
        entry="$(printf '%s:%s: %s' "$f" "$ln" "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')")"
        skip=0
        for pat in "${allow_pats[@]:-}"; do
            [ -n "$pat" ] || continue
            printf '%s' "$entry" | grep -qiE "$pat" && { skip=1; break; }
        done
        [ "$skip" = 1 ] && continue
        bad=1
        _offenders+=("$entry")
    done <<< "$hits"
done

if [ "$bad" = 0 ]; then
    printf 'payload-argv-lint: clean — %d file(s) checked\n' "${#files[@]}"
    exit 0
fi
cat >&2 <<'WHY'

REFUSED by payload-argv-lint.sh — the lines above hand a JSON-named variable to
python3/jq/awk through argv or an environment variable. A store-sized payload crosses
MAX_ARG_STRLEN (128 KiB) silently: the exec dies, the caller reads empty output, and empty
reads as "nothing to do" (law-payloads-go-on-stdin). Five outages in seventeen days.

Pass the payload on stdin, or write it to a temp file and pass the PATH (a short string) as
the argument/env value instead. See epic_rank_rows / queue_sort_rows in lib.sh for the
pattern. A genuine exception belongs in spira/payload-argv-lint-allow, not a workaround here.
WHY
printf '%s\n' "${_offenders[@]}"
exit 1
