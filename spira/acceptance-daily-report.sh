#!/usr/bin/env bash
# acceptance-daily-report.sh — turn a red acceptance-daily run into GitHub issues.
#
# Usage: acceptance-daily-report.sh --log FILE --sha SHA --run-url URL [--repo OWNER/NAME]
#
# One issue per distinct failing check (title "acceptance-daily: <check>"), at most
# MAX_ISSUES per run. An open issue with that title gets a comment instead of a second issue.
# A log with no FAIL line (the run never reached a verdict) files one issue of its own with
# the log tail, so a run that could not start is as loud as one that failed.
#
# THE TRUST ROUTE. GH_TOKEN is the workflow's own GITHUB_TOKEN (issues: write), so the author is
# the repo's github-actions[bot] and the issue carries `spira:acceptance`; gh-intake trusts exactly
# that combination. Without a token nothing is filed silently: the script exits 1.
set -uo pipefail

MAX_ISSUES="${ACCEPTANCE_DAILY_MAX_ISSUES:-5}"
LABEL="acceptance-daily"
_log=""; _sha=""; _url=""; _repo="${GITHUB_REPOSITORY:-}"
while [ $# -gt 0 ]; do
    case "$1" in
        --log)     _log="${2:-}"; shift 2 ;;
        --sha)     _sha="${2:-}"; shift 2 ;;
        --run-url) _url="${2:-}"; shift 2 ;;
        --repo)    _repo="${2:-}"; shift 2 ;;
        *) printf 'acceptance-daily-report: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
[ -f "$_log" ] && [ -n "$_sha" ] && [ -n "$_url" ] && [ -n "$_repo" ] || {
    printf 'usage: acceptance-daily-report.sh --log FILE --sha SHA --run-url URL [--repo OWNER/NAME]\n' >&2
    exit 2
}
[ -n "${GH_TOKEN:-}" ] || {
    printf 'acceptance-daily-report: ALARM: no GH_TOKEN — a red run cannot be filed as trusted work\n' >&2
    exit 1
}

_fails="$(grep -E '^[[:space:]]*FAIL[[:space:]]' "$_log" | sed -E 's/^[[:space:]]+//' | sort -u)"
_gh() { timeout 5 gh "$@"; }

_ensure_labels() {
    _gh label create "$LABEL" --repo "$_repo" --color B60205 --description "daily fresh-install acceptance red" >/dev/null 2>&1 || true
    _gh label create spira:acceptance --repo "$_repo" --color 0E8A16 --description "promote to work" >/dev/null 2>&1 || true
}

_open_numbers() {
    _gh issue list --repo "$_repo" --label "$LABEL" --state open --limit 100 --json number,title \
        --jq '.[] | "\(.number)\t\(.title)"'
}

_file() {
    local title="$1" lines="$2" body number
    body="$(printf 'Daily fresh-install acceptance (phase A, no predecessor) is red.\n\n- commit: `%s`\n- run: %s\n\nFailing lines, verbatim:\n\n```\n%s\n```\n' "$_sha" "$_url" "$lines")"
    number="$(printf '%s\n' "$_open" | awk -F'\t' -v t="$title" '$2==t {print $1; exit}')"
    if [ -n "$number" ]; then
        printf '%s\n' "$body" | _gh issue comment "$number" --repo "$_repo" --body-file - >/dev/null || return 1
        printf 'acceptance-daily-report: appended to #%s (%s)\n' "$number" "$title"
    else
        number="$(printf '%s\n' "$body" | _gh issue create --repo "$_repo" --title "$title" --label "$LABEL" --label spira:acceptance --body-file -)" || return 1
        number="${number##*/}"
        case "$number" in
            ''|*[!0-9]*) printf 'acceptance-daily-report: ALARM: cannot read the number of the issue just filed (%s)\n' "$title" >&2; return 1 ;;
        esac
        _open="$_open"$'\n'"$number"$'\t'"$title"
        printf 'acceptance-daily-report: filed %s\n' "$title"
    fi
}

_ensure_labels
_open="$(_open_numbers)" || { printf 'acceptance-daily-report: ALARM: cannot list open issues\n' >&2; exit 1; }

_rc=0
if [ -z "$_fails" ]; then
    _file "acceptance-daily: run did not reach a verdict" "$(tail -n 40 "$_log")" || _rc=1
else
    _n=0; _seen=""
    while IFS= read -r _line; do
        [ -n "$_line" ] || continue
        _name="$(printf '%s' "$_line" | sed -E 's/^FAIL[[:space:]]+//; /^phase [A-Za-z0-9]+: /{s/^(phase [A-Za-z0-9]+: [^:]*).*$/\1/;b;}; s/:.*$//')"
        case "$_seen" in *"|$_name|"*) continue ;; esac
        _seen="$_seen|$_name|"; _n=$((_n + 1))
        [ "$_n" -le "$MAX_ISSUES" ] || continue
        _file "acceptance-daily: $_name" "$(printf '%s\n' "$_fails" | grep -F -- "FAIL  $_name:" || printf '%s' "$_line")" || _rc=1
    done <<<"$_fails"
    [ "$_n" -le "$MAX_ISSUES" ] || printf 'acceptance-daily-report: %s failing checks, filed the first %s; the rest are in the run log\n' "$_n" "$MAX_ISSUES" >&2
fi
exit "$_rc"
