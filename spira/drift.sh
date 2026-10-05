#!/usr/bin/env bash
#
# drift.sh — does the running system still match what git and this install's own manifest say
# it should?
#
#   drift.sh checkout [<repo>]     untracked or modified paths in a production checkout
#   drift.sh units    [<unitdir>]  installed unit files and drop-ins this install does not ship
#   drift.sh check    [<repo>] [<unitdir>]   both, combined
#
# Operator-local overrides declared in $SPIRA_LOCAL_UNITS ([[unit]] path/reason/bead) are
# accepted by `units`; one whose retiring bead has landed is reported RETIRE-NOW.
#
# WHY THIS EXISTS
# ----------------
# git, and this install's own unit manifest, are not what the running system reads: a suite
# lister that enumerates from disk, or systemd reading whatever sits in the unit directory,
# both execute a stray file exactly as if it were shipped. Twice here that stray file was
# diagnosed as something else entirely before anyone thought to ask git (sp-81ph9, sp-39gqq).
#
# EXIT   0  clean
#        1  drift found — reported on stdout, one finding per line
#        3  could not check — said out loud, never a silent pass
#              (law-absence-needs-a-positive-control)
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/conf.sh"

# checkout <repo> -> every untracked or modified path against git, one DIRTY line each.
#
# `git status --porcelain` alone: it already honours .gitignore for untracked files, so
# runtime output the checkout itself writes is not drift, and a modified TRACKED file is
# exactly as much drift as an untracked one — sp-81ph9's stray suite would have shown here
# on the first look.
checkout() {
    local repo="${1:-${SPIRA_REPO:-}}"
    [ -n "$repo" ] || {
        echo "drift: no repository given and SPIRA_REPO is unset" >&2; return 3; }
    [ -e "$repo/.git" ] || {
        echo "drift: $repo is not a git checkout" >&2; return 3; }
    local out
    out="$(git -C "$repo" status --porcelain 2>/dev/null)" || {
        echo "drift: git status failed in $repo" >&2; return 3; }
    if [ -z "$out" ]; then
        echo "drift: checkout clean — $repo matches git"
        return 0
    fi
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        printf 'DIRTY %s (%s)\n' "$line" "$repo"
    done <<< "$out"
    return 1
}

# local_units <manifest> -> "<path>\t<bead>" per complete [[unit]] table. An entry without a
# retiring bead is not a declaration: an interim must name what ends it.
local_units() {
    local file="$1"
    [ -n "$file" ] && [ -r "$file" ] || return 0
    awk '
        function flush() { if (path != "" && bead != "") printf "%s\t%s\n", path, bead; path = ""; bead = "" }
        /^[[:space:]]*\[\[unit\]\]/ { flush(); next }
        /^[[:space:]]*path[[:space:]]*=/ { v = $0; sub(/^[^=]*=[[:space:]]*"/, "", v); sub(/"[[:space:]]*(#.*)?$/, "", v); path = v }
        /^[[:space:]]*bead[[:space:]]*=/ { v = $0; sub(/^[^=]*=[[:space:]]*"/, "", v); sub(/"[[:space:]]*(#.*)?$/, "", v); bead = v }
        END { flush() }
    ' "$file"
}

# declared_verdict <unitdir> <file> <why> -> prints a finding unless <file> is a declared
# local override whose retiring bead has not landed. Returns 1 when it printed one.
declared_verdict() {
    local unitdir="$1" f="$2" why="$3" rel bead
    rel="${f#"$unitdir"/}"
    bead="${declared[$rel]:-}"
    if [ -z "$bead" ]; then
        printf 'UNSHIPPED %s (%s)\n' "$f" "$why"
        return 1
    fi
    if [ "$(spira-lc state "$bead" 2>/dev/null)" = LANDED ]; then
        printf 'RETIRE-NOW %s (declared local override; retiring bead %s has landed)\n' "$f" "$bead"
        return 1
    fi
    return 0
}

# units <unitdir> -> every installed unit file or drop-in this install's own manifest
# (owned.sh) does not declare, one UNSHIPPED line each.
#
# owned.sh list IS THE MANIFEST, not a second copy of it: it already resolves the same
# UNITS/OPTIONAL/UNBUILT arrays install.sh renders from, so a unit this tree would install
# is never reported unshipped just because this checkout happens not to have built it.
#
# DROP-INS ARE A SEPARATE QUESTION. owned.sh declares alert-dropin locations, but a drop-in
# is legitimate two ways: intake's 50-spira-intake.conf (install-intake.sh's DROPIN) and
# cadence's override (cadence.sh's DROPIN_FILE). Anything else in a *.d/ directory here was
# not written by a mechanism this repo ships — sp-39gqq's refuse-manual-stop.conf is exactly
# that shape.
units() {
    local unitdir="${1:-$HOME/.config/systemd/user}"
    [ -d "$unitdir" ] || {
        echo "drift: $unitdir does not exist — nothing installed to check" >&2; return 3; }
    local owned_sh="$HERE/owned.sh"
    [ -r "$owned_sh" ] || {
        echo "drift: $owned_sh is missing — cannot read this install's manifest" >&2; return 3; }

    local manifest
    manifest="$(bash "$owned_sh" list 2>/dev/null)" || {
        echo "drift: owned.sh list failed — cannot read this install's manifest" >&2; return 3; }
    [ -n "$manifest" ] || {
        echo "drift: owned.sh list produced nothing — cannot read this install's manifest" >&2
        return 3
    }

    local -A expected_units=()
    local kind id loc phase retention
    while IFS='|' read -r kind id loc phase retention; do
        [ "$kind" = unit ] || continue
        expected_units["$(basename "$loc")"]=1
    done <<< "$manifest"

    local -A known_dropins=([50-spira-intake.conf]=1 [cadence.conf]=1)

    local -A declared=()
    local dpath dbead
    while IFS=$'\t' read -r dpath dbead; do
        [ -n "$dpath" ] && declared["$dpath"]="$dbead"
    done < <(local_units "${SPIRA_LOCAL_UNITS:-}")

    local rc=0 f base
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        base="$(basename "$f")"
        if [ -z "${expected_units[$base]:-}" ]; then
            declared_verdict "$unitdir" "$f" "unit file this install does not ship" || rc=1
        fi
    done < <(find "$unitdir" -maxdepth 1 \( -name '*.service' -o -name '*.timer' -o -name '*.socket' \) -type f 2>/dev/null | sort)

    while IFS= read -r f; do
        [ -n "$f" ] || continue
        base="$(basename "$f")"
        if [ -z "${known_dropins[$base]:-}" ]; then
            declared_verdict "$unitdir" "$f" "drop-in no repo mechanism installs" || rc=1
        fi
    done < <(find "$unitdir" -mindepth 2 -maxdepth 2 -path '*.d/*.conf' -type f 2>/dev/null | sort)

    [ "$rc" = 0 ] && echo "drift: units clean — $unitdir matches what this install ships"
    return "$rc"
}

# check [<repo>] [<unitdir>] -> both checks, combined. 3 outranks 1: a check that could not
# run at all is a worse answer than one that ran and found something (law-fail-closed-at-the-source).
check() {
    local repo="${1:-${SPIRA_REPO:-}}" unitdir="${2:-$HOME/.config/systemd/user}"
    local co_out un_out co_rc un_rc
    co_out="$(checkout "$repo")"; co_rc=$?
    un_out="$(units "$unitdir")"; un_rc=$?
    printf '%s\n' "$co_out"
    printf '%s\n' "$un_out"
    if [ "$co_rc" = 3 ] || [ "$un_rc" = 3 ]; then return 3; fi
    if [ "$co_rc" = 1 ] || [ "$un_rc" = 1 ]; then return 1; fi
    return 0
}

case "${1:-check}" in
    checkout) shift; checkout "$@" ;;
    units)    shift; units "$@" ;;
    check)    shift; check "$@" ;;
    *)        sed -n '2,7p' "$0" >&2; exit 2 ;;
esac
