#!/usr/bin/env bash
# acceptance-ci.sh — shell-only steps of the acceptance workflow.
# Tested by test-acceptance-ci.sh; called by acceptance.yml.
#
# Usage: acceptance-ci.sh <tag> [--prev-tag <t>] [--bd-db <path>]
#                                [--agent <path>] [--record]
#
# Override acceptance-run.sh for testing: SPIRA_ACCEPTANCE_RUN=<path>
set -uo pipefail

_tag=""
_prev_tag=""
_bd_db="$HOME/.local/share/spira/db"
_agent=""
_do_record=0

while [ $# -gt 0 ]; do
    case "$1" in
        --prev-tag)   _prev_tag="${2:-}"; shift 2 ;;
        --prev-tag=*) _prev_tag="${1#--prev-tag=}"; shift ;;
        --bd-db)      _bd_db="${2:-}"; shift 2 ;;
        --bd-db=*)    _bd_db="${1#--bd-db=}"; shift ;;
        --agent)      _agent="${2:-}"; shift 2 ;;
        --agent=*)    _agent="${1#--agent=}"; shift ;;
        --record)     _do_record=1; shift ;;
        -*)           printf 'acceptance-ci: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)
            if [ -z "$_tag" ]; then _tag="$1"
            else printf 'acceptance-ci: too many positional arguments\n' >&2; exit 2
            fi
            shift ;;
    esac
done

[ -n "$_tag" ] || {
    printf 'usage: acceptance-ci.sh <tag> [--prev-tag <t>] [--bd-db <path>]\n' >&2
    exit 2
}

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
export PATH="$HOME/.local/bin:$PATH"

git init --bare --initial-branch=main "$HOME/scratch-repo.git"
git clone "$HOME/scratch-repo.git" "$HOME/scratch-repo"
git -C "$HOME/scratch-repo" \
    -c user.email="acceptance@spira.local" \
    -c user.name="Acceptance" \
    commit --allow-empty -m "init"
git -C "$HOME/scratch-repo" push origin main

mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/spira"
printf 'scratch-repo | %s | push | origin/main | |\n' "$HOME/scratch-repo" \
    > "${XDG_CONFIG_HOME:-$HOME/.config}/spira/repo-map"

_run_args=("$_tag" --scratch-repo "$HOME/scratch-repo" --bd-db "$_bd_db")
[ -n "$_agent" ]        && _run_args+=(--agent "$_agent")
[ "$_do_record" -eq 1 ] && _run_args+=(--record)
[ -n "$_prev_tag" ]     && _run_args+=(--prev-tag "$_prev_tag")

printf '--- runner env ---\n'
printf 'whoami: %s\n' "$(whoami 2>/dev/null || echo unknown)"
printf 'HOME:   %s\n' "$HOME"
printf 'PATH:   %s\n' "$PATH"
printf 'bd:     %s\n' "$(command -v bd 2>/dev/null || echo not-on-PATH)"
ls -l "$HOME/.local/bin" 2>/dev/null || printf '  (.local/bin absent)\n'
file "$HOME/.local/bin/bd" 2>/dev/null || printf '  (file: not found)\n'
"$HOME/.local/bin/bd" --version 2>/dev/null || printf '  (bd --version failed)\n'
findmnt -T "$HOME/.local/bin" -o TARGET,OPTIONS 2>/dev/null || printf '  (findmnt unavailable)\n'
printf '--- end ---\n'

_run_rc=0
bash "${SPIRA_ACCEPTANCE_RUN:-$HERE/acceptance-run.sh}" "${_run_args[@]}" || _run_rc=$?

if [ -n "${GH_TOKEN:-}" ]; then
    git config user.email "acceptance@spira.local" 2>/dev/null || true
    git config user.name "Spira Acceptance" 2>/dev/null || true
    git push origin 'refs/notes/acceptance' 2>/dev/null || true
fi

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    _note="$(git notes --ref=acceptance show "refs/tags/$_tag" 2>/dev/null \
        || printf '(no note written)')"
    printf '## Acceptance: %s\n\n```\n%s\n```\n' "$_tag" "$_note" \
        >> "$GITHUB_STEP_SUMMARY"
fi

exit "$_run_rc"
