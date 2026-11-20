#!/usr/bin/env bash
#
# test-release-symbols.sh — the release profile keeps function names and drops debug info.
#
# CASES
#   1. nm on spira-lc lists spira_lc::main and sqlite3_step.
#   2. readelf lists no .debug_info section (a non-ELF input is refused, not passed).
#   3. POSITIVE CONTROL: a --strip-all copy fails the case-1 check.
#
# tier: T1
# covers: Cargo.toml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-release-symbols.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

for t in nm readelf strip spira-lc; do
    command -v "$t" >/dev/null 2>&1 || { bad "$t is on PATH" "missing: $t"; tl_summary; exit 1; }
done
LC="$(command -v spira-lc)"

symbols_present() {
    local out
    out="$(nm -C "$1" 2>&1)" || return 1
    grep -q ' spira_lc::main$' <<<"$out" || return 1
    grep -q ' sqlite3_step$' <<<"$out" || return 1
}

if symbols_present "$LC"; then ok "spira-lc keeps spira_lc::main and sqlite3_step"
else bad "spira-lc keeps spira_lc::main and sqlite3_step" "$(nm "$LC" 2>&1 | head -1)"; fi

has_debug_info() {
    local out
    out="$(readelf -S -W "$1" 2>&1)" || return 2
    grep -q ' \.text ' <<<"$out" || return 2
    grep -q ' \.debug_info ' <<<"$out"
}

has_debug_info "$LC"; rc=$?
is "spira-lc has no .debug_info section" "1" "$rc"

cp "$LC" "$TMP/stripped" && strip --strip-all "$TMP/stripped"
if symbols_present "$TMP/stripped"; then bad "a fully stripped copy fails the symbol check" "check passed"
else ok "a fully stripped copy fails the symbol check"; fi

printf 'x' > "$TMP/notelf"; has_debug_info "$TMP/notelf"; rc=$?
is "a non-ELF input is refused, not read as no debug info" "2" "$rc"

tl_summary
