#!/usr/bin/env bash
#
# release-order.sh — the one ordering of release tags, read from stdin (one tag per line).
#
#   release-order.sh sort      tags in ascending version order
#   release-order.sh latest    the newest tag (empty when none)
#   release-order.sh next      the N for the next spira-<series>.N (needs the published
#                              release list; the total count and the highest N both floor it)
#   release-order.sh stem <t>  the tarball stem of a tag
#   release-order.sh tag <s>   the tag of a tarball stem
#
# Two forms exist: the legacy spira-release-<repo>-<UTCstamp>[-k] and spira-<major>.<minor>.<N>.
# Every legacy tag sorts before every versioned one; legacy tags order by name, versioned by
# number. Anything matching neither form is ignored.
set -uo pipefail

_ro_filter() {
    awk '
        /^spira-release-.+-[0-9]{8}T[0-9]{6}Z(-[0-9]+)?$/ { printf "0 %s %s\n", $0, $0; next }
        /^spira-[0-9]+\.[0-9]+\.[0-9]+$/ {
            split(substr($0, 7), v, ".")
            printf "1 %09d.%09d.%09d %s\n", v[1], v[2], v[3], $0
        }'
}

_ro_sorted() { _ro_filter | LC_ALL=C sort -k1,1 -k2,2 | awk '{print $3}'; }

case "${1:-}" in
    sort)   _ro_sorted ;;
    latest) _ro_sorted | tail -1 ;;
    next)
        _ro_all="$(_ro_sorted)"
        _ro_count="$(printf '%s' "$_ro_all" | grep -c . || true)"
        _ro_max="$(printf '%s\n' "$_ro_all" | sed -n 's/^spira-[0-9]*\.[0-9]*\.\([0-9]*\)$/\1/p' | sort -n | tail -1)"
        _ro_base=$(( _ro_count > ${_ro_max:-0} ? _ro_count : ${_ro_max:-0} ))
        printf '%s\n' $(( _ro_base + 1 )) ;;
    stem)
        case "${2:-}" in
            spira-release-*) printf '%s\n' "${2#spira-release-}" | sed -E 's/^.*-([0-9]{8}T[0-9]{6}Z(-[0-9]+)?)$/spira-\1/' ;;
            spira-[0-9]*) printf '%s\n' "$2" ;;
            *) exit 1 ;;
        esac ;;
    tag)
        case "${2:-}" in
            spira-[0-9]*.[0-9]*.[0-9]*) printf '%s\n' "$2" ;;
            spira-[0-9]*T*Z*) printf 'spira-release-spira-%s\n' "${2#spira-}" ;;
            *) exit 1 ;;
        esac ;;
    *) printf 'usage: release-order.sh sort|latest|next|stem <tag>|tag <stem>\n' >&2; exit 2 ;;
esac
