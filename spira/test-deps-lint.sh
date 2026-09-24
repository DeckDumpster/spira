#!/usr/bin/env bash
#
# test-deps-lint.sh — deps-lint.sh refuses undeclared external programs; the
# shipped tree is clean.
#
# covers: spira/deps-lint.sh spira/deps.toml spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { printf '%s' "$3" | grep -qF "$2" && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-deps-lint.sh"

lint() { bash "$HERE/deps-lint.sh" "$@" 2>&1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL: the lint must fire before we trust its silence.
# Plant an undeclared program in a scratch tree and require the lint to name it.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "POSITIVE CONTROL — undeclared program in shell script:"

ROOT_SH="$TMP/root-sh"
mkdir -p "$ROOT_SH/spira"
# Copy the real deps.toml and the lint itself into the scratch tree.
cp "$HERE/deps.toml"    "$ROOT_SH/spira/deps.toml"
cp "$HERE/deps-lint.sh" "$ROOT_SH/spira/deps-lint.sh"
# Plant a shell script that checks for a program not in deps.toml.
printf '#!/bin/sh\ncommand -v spira-no-such-prog-xyz >/dev/null\n' \
    > "$ROOT_SH/spira/planted.sh"

out="$(lint "$ROOT_SH")"; rc=$?
is   "SEEN RED: undeclared program in shell → exit 1" "1" "$rc"
want "and it names the program" "spira-no-such-prog-xyz" "$out"
want "and it names the file"    "planted.sh"             "$out"

# Withdraw the plant — the lint should now pass.
rm "$ROOT_SH/spira/planted.sh"
out="$(lint "$ROOT_SH" 2>&1)"; rc=$?
is "SEEN GREEN: plant removed → exit 0" "0" "$rc"

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL: undeclared program in a Rust source.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "POSITIVE CONTROL — undeclared program in Rust source:"

ROOT_RS="$TMP/root-rs"
mkdir -p "$ROOT_RS/spira" "$ROOT_RS/src"
cp "$HERE/deps.toml"    "$ROOT_RS/spira/deps.toml"
cp "$HERE/deps-lint.sh" "$ROOT_RS/spira/deps-lint.sh"
printf 'fn main() { let _ = std::process::Command::new("spira-no-such-prog-xyz"); }\n' \
    > "$ROOT_RS/src/main.rs"

out="$(lint "$ROOT_RS")"; rc=$?
is   "SEEN RED: undeclared Rust literal → exit 1" "1" "$rc"
want "and it names the program" "spira-no-such-prog-xyz" "$out"
want "and it names the file"    "main.rs"                "$out"

rm "$ROOT_RS/src/main.rs"
out="$(lint "$ROOT_RS" 2>&1)"; rc=$?
is "SEEN GREEN: plant removed → exit 0" "0" "$rc"

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL: empty/missing manifest is refused, not reported as clean.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "POSITIVE CONTROL — missing manifest refuses to report clean:"

ROOT_NODEPS="$TMP/root-nodeps"
mkdir -p "$ROOT_NODEPS/spira"
cp "$HERE/deps-lint.sh" "$ROOT_NODEPS/spira/deps-lint.sh"
# No deps.toml — lint should exit 2 (config error), not 0 (clean).
out="$(bash "$ROOT_NODEPS/spira/deps-lint.sh" "$ROOT_NODEPS" 2>&1)"; rc=$?
is "missing deps.toml exits 2" "2" "$rc"

# ──────────────────────────────────────────────────────────────────────────────
# THE SHIPPED TREE IS CLEAN.
# ──────────────────────────────────────────────────────────────────────────────
echo
echo "the shipped tree is clean:"

out="$(lint 2>&1)"; rc=$?
is "shipped tree exits 0" "0" "$rc"
[ -z "$out" ] && ok "no offenders reported" \
              || bad "no offenders reported" "got: $out"

echo
echo "test-deps-lint.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
