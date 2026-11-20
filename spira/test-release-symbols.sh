#!/usr/bin/env bash
#
# test-release-symbols.sh — the release profile keeps function names and drops debug info.
#
# CASES
#   1. nm on spira-lc lists spira_lc::main and sqlite3_step.
#   2. the workspace release profile says strip = "debuginfo" (a binary built under dev carries
#      debug info, so the binary cannot judge this; the declared profile does).
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

for t in nm strip spira-lc; do
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

release_strip_value() {
    awk '/^\[/ { in_rel = ($0 == "[profile.release]") } in_rel && /^strip[ ]*=/ { sub(/^strip[ ]*=[ ]*/, ""); print; exit }' "$1"
}

is "the release profile strips debuginfo only" '"debuginfo"' "$(release_strip_value "$HERE/../Cargo.toml")"

printf '[profile.release]\nstrip = true\n[profile.dev]\nstrip = "debuginfo"\n' > "$TMP/Cargo.toml"
is "the profile reader tells strip = true from debuginfo" "true" "$(release_strip_value "$TMP/Cargo.toml")"

cp "$LC" "$TMP/stripped" && strip --strip-all "$TMP/stripped"
if symbols_present "$TMP/stripped"; then bad "a fully stripped copy fails the symbol check" "check passed"
else ok "a fully stripped copy fails the symbol check"; fi

tl_summary
