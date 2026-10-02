#!/usr/bin/env bash
#
# verify-landed.sh — run the production checks beads declared, after a release activates.
#
#   verify-landed.sh [--range <A>..<B> [--repo <path>]] [<bead-id>...]
#
# A bead declares its check with a body line:
#
#   verify: <command>                      passes on exit 0
#   verify: <command> => <expected text>   passes on exit 0 AND output containing the text
#
# Each named bead, plus every bead id found in the commit subjects of --range, is checked
# once per release: the command runs bounded (SPIRA_VERIFY_TIMEOUT) in this process's
# environment. A pass is recorded as a comment on the closed bead. A failure files a child
# bead (<id>.N) carrying the command and its output; the checked bead is never reopened.
#
# A bead with no verify: line is skipped silently. A check that already has a recorded
# result for this release is not run again, so a re-run files nothing twice.
#
# covers: spira/verify-landed.sh spira/deploy.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

BD="${SPIRA_BD:-bd}"
BEAD="${SPIRA_VERIFY_BEAD_SH:-$HERE/bead.sh}"
TIMEOUT="${SPIRA_VERIFY_TIMEOUT:-120}"
RANGE=""; REPO="${SPIRA_REPO:-}"; ids=()
while [ $# -gt 0 ]; do
    case "$1" in
        --range) RANGE="${2:-}"; shift ;;
        --repo) REPO="${2:-}"; shift ;;
        -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
        -*) printf 'verify-landed.sh: unknown flag %s\n' "$1" >&2; exit 2 ;;
        *) ids+=("$1") ;;
    esac
    shift
done

if [ -n "$RANGE" ]; then
    [ -n "$REPO" ] || { printf 'verify-landed.sh: --range needs --repo or SPIRA_REPO\n' >&2; exit 2; }
    subjects="$(git -C "$REPO" log --format=%s "$RANGE" 2>/dev/null)" || {
        printf 'verify-landed.sh: cannot read range %s in %s\n' "$RANGE" "$REPO" >&2; exit 1; }
    while read -r id; do ids+=("$id"); done < <(printf '%s\n' "$subjects" \
        | grep -oE "\b${SPIRA_ID_PREFIX:-sp}-[a-z0-9]+\b" | sort -u)
fi

release="${RANGE##*..}"; release="${release:-unspecified}"
marker="post-deploy verify [$release]"
checked=0; failed=0; rc=0

for id in $(printf '%s\n' "${ids[@]}" | sort -u); do
    shown="$("$BD" -C "$SPIRA_DB" show "$id" --json 2>/dev/null \
        | python3 -c 'import json,sys
d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d
print(next((l[5:] for l in d.get("labels") or [] if l.startswith("repo:")), ""))
print(d.get("description") or "")' 2>/dev/null)" || continue
    repo="${shown%%$'\n'*}"; body="${shown#*$'\n'}"
    line="$(printf '%s\n' "$body" | sed -n 's/^[[:space:]]*verify:[[:space:]]*//p' | head -n 1)"
    [ -n "$line" ] || continue
    cmd="${line%% => *}"; want=""
    case "$line" in *" => "*) want="${line#* => }" ;; esac
    if "$BD" -C "$SPIRA_DB" comments "$id" 2>/dev/null | grep -qF "$marker"; then
        printf 'verify-landed: %s already checked for %s\n' "$id" "$release"; continue
    fi
    checked=$((checked+1))
    out="$(timeout "$TIMEOUT" bash -c "$cmd" 2>&1)"; crc=$?
    ok=1
    [ "$crc" -eq 0 ] || ok=0
    if [ "$ok" = 1 ] && [ -n "$want" ]; then
        case "$out" in *"$want"*) ;; *) ok=0 ;; esac
    fi
    short="$(printf '%s' "$out" | head -c 2000)"
    if [ "$ok" = 1 ]; then
        printf 'verify-landed: PASS %s\n' "$id"
        "$BD" -C "$SPIRA_DB" comments add "$id" \
            "$marker PASS: $cmd -> exit $crc. Output: $short" >/dev/null 2>&1 || rc=1
    else
        failed=$((failed+1))
        printf 'verify-landed: FAIL %s (exit %s)\n' "$id" "$crc"
        bodyf="$(mktemp)"
        {
            printf 'The production check declared by %s failed after release %s.\n\n' "$id" "$release"
            printf 'command: %s\n' "$cmd"
            [ -z "$want" ] || printf 'expected output to contain: %s\n' "$want"
            printf 'exit: %s\n\noutput:\n%s\n' "$crc" "$short"
        } > "$bodyf"
        "$BEAD" file "fix of $id did not hold in production: verify failed" \
            --for builder --repo "$repo" --priority 1 \
            --parent "$id" --body-file "$bodyf" >/dev/null || rc=1
        rm -f "$bodyf"
        "$BD" -C "$SPIRA_DB" comments add "$id" \
            "$marker FAIL: $cmd -> exit $crc; follow-up filed." >/dev/null 2>&1 || rc=1
    fi
done
printf 'verify-landed: %s check(s) run, %s failed\n' "$checked" "$failed"
exit "$rc"
