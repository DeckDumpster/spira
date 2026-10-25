#!/usr/bin/env bash
#
# release-notes.sh — generate a release body from the landed beads between two commits.
#
#   release-notes.sh --prev <ref> --this <ref> [--tag T] [--prev-tag P] [--repo <dir>] [--run-url U]
#
# Commit subjects of the form "<prefix>: land <bead> — <title>" are the changes; the paths
# each touched decide its area. Config keys, units and migrations are read from the diff.
# Output goes through a scrubber: paths under /home, the host name and every line of
# spira/inventory-deny never leave this script.
set -uo pipefail

prev="" this="" tag="" prev_tag="" repo="." run_url=""
while [ $# -gt 0 ]; do
    case "$1" in
        --prev) prev="$2"; shift 2 ;;
        --this) this="$2"; shift 2 ;;
        --tag) tag="$2"; shift 2 ;;
        --prev-tag) prev_tag="$2"; shift 2 ;;
        --repo) repo="$2"; shift 2 ;;
        --run-url) run_url="$2"; shift 2 ;;
        *) printf 'release-notes: unknown argument %s\n' "$1" >&2; exit 2 ;;
    esac
done
[ -n "$prev" ] && [ -n "$this" ] || { printf 'release-notes: --prev and --this are required\n' >&2; exit 2; }
g() { git -C "$repo" "$@"; }
g rev-parse --verify -q "$this^{commit}" >/dev/null || { printf 'release-notes: no commit %s\n' "$this" >&2; exit 1; }
g rev-parse --verify -q "$prev^{commit}" >/dev/null || { printf 'release-notes: no commit %s\n' "$prev" >&2; exit 1; }

AREAS=(
    "Queue and rounds|batcher queue landing-pass gate round sift sentinel refinery"
    "Lifecycle|lifecycle bead work spira-claim spira-submission"
    "Testenv and simulation|testenv sim testkit spira-world test-plan suite-select"
    "Cockpit and ops pane|cockpit watchd watchtower census"
    "Habits and personas|habits chamber aeon persona"
    "Install and release|release install .github build-tarball"
    "Docs|docs README AGENTS CLAUDE wiki"
)
area_of() {
    local best="Other" bestn=0 spec name words w n f
    for spec in "${AREAS[@]}"; do
        name="${spec%%|*}"; words="${spec#*|}"; n=0
        for f in "$@"; do
            for w in $words; do
                case "$f" in "$w"/*|*/"$w"/*|*/"$w"*|"$w"*) n=$((n + 1)); break ;; esac
            done
        done
        [ "$n" -gt "$bestn" ] && { best="$name"; bestn=$n; }
    done
    printf '%s' "$best"
}

scrub() {
    local host sed_args=(-E -e 's#/(home|root|Users)/[^[:space:])`"'"'"']*#<path>#g')
    for host in "$(hostname -s 2>/dev/null)" "$(hostname -f 2>/dev/null)" "$(hostname 2>/dev/null)"; do
        [ -n "$host" ] && sed_args+=(-e "s#$(printf '%s' "$host" | sed 's/[.[\*^$#]/\\&/g')#<host>#Ig")
    done
    local deny="$repo/spira/inventory-deny" pat
    if [ -f "$deny" ]; then
        while IFS= read -r pat; do
            case "$pat" in ''|'#'*) continue ;; esac
            sed_args+=(-e "s#${pat//#/\\#}#<redacted>#Ig")
        done < "$deny"
    fi
    sed "${sed_args[@]}"
}

generate() {
    local re='^[^:]+: land (sp-[a-z0-9]+(\.[0-9]+)?) — (.*)$'
    declare -A bucket
    local n=0 sha subj id title kind area files
    while IFS=$'\t' read -r sha subj; do
        [[ "$subj" =~ $re ]] || continue
        id="${BASH_REMATCH[1]}"; title="${BASH_REMATCH[3]}"
        mapfile -t files < <(g diff-tree --no-commit-id --name-only -r -m --first-parent "$sha" 2>/dev/null)
        area="$(area_of "${files[@]}")"
        kind=""
        [[ "${title,,}" =~ (^|[^a-z])(fix|fixes|fixed)([^a-z]|$) ]] && kind=" (fix)"
        bucket["$area"]+="- $id — $title$kind"$'\n'
        n=$((n + 1))
    done < <(g log --first-parent --format='%H%x09%s' "$prev..$this")

    local ver="${tag:-$(g rev-parse --short "$this")}"
    printf '# %s\n\n' "$ver"
    printf 'Commit %s, %s. %d change%s since %s.\n\n' "$(g rev-parse --short "$this")" \
        "$(g log -1 --format=%cs "$this")" "$n" "$([ "$n" = 1 ] || echo s)" \
        "${prev_tag:-$(g rev-parse --short "$prev")}"

    printf '## What changed\n\n'
    if [ "$n" = 0 ]; then printf 'No landed beads in this range.\n\n'; fi
    local a
    for a in "Queue and rounds" "Lifecycle" "Testenv and simulation" "Cockpit and ops pane" \
             "Habits and personas" "Install and release" "Docs" "Other"; do
        [ -n "${bucket[$a]:-}" ] || continue
        printf '### %s\n\n%s\n' "$a" "${bucket[$a]}"
    done

    printf '## Upgrade notes\n\n'
    local line any
    section() {
        local label="$1" status="$2" pathspec="$3" strip="$4" out
        out="$(g diff --name-status --diff-filter="$status" "$prev" "$this" -- "$pathspec" | cut -f2- | sed "s#$strip##" | sort)"
        [ -n "$out" ] || return 0
        any=1
        printf '**%s**\n\n' "$label"
        while IFS= read -r line; do printf -- '- `%s`\n' "$line"; done <<< "$out"
        printf '\n'
    }
    any=""
    section "Config keys added" A "spira/conf.d" "spira/conf.d/"
    section "Config keys removed" D "spira/conf.d" "spira/conf.d/"
    section "Systemd units/timers added" A "systemd/*.service systemd/*.timer" "systemd/"
    section "Systemd units/timers removed" D "systemd/*.service systemd/*.timer" "systemd/"
    section "Schema migrations added" A "*/migrations/*.sql" ""
    [ -n "$any" ] || printf 'No config key, unit or migration changes.\n\n'

    printf '## Verification\n\n'
    printf 'Landed on the landing ref through the batch rounds; the %d commits above are the range `%s..%s`.\n' \
        "$n" "$(g rev-parse --short "$prev")" "$(g rev-parse --short "$this")"
    if [ -n "$run_url" ]; then
        printf 'Acceptance run for this commit: %s\n' "$run_url"
    fi
}

generate | scrub
