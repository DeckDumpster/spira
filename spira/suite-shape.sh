#!/usr/bin/env bash
# suite-shape.sh — classify a spira test suite by the host-access pattern it uses.
#
# Each shape maps to a migration wave:
#   cp-a-tree, stub      → wave 2  (fake-install suites; drift when file lists go stale)
#   home-writer, real-git, real-systemd → wave 3
#   testdb, hermetic, rust-test, unknown → no wave yet
#
# Usage:
#   spira/suite-shape.sh spira/test-<name>.sh
#
# Output: space-separated list of shape labels (cp-a-tree, stub, home-writer,
#         real-git, testdb, real-systemd, hermetic, rust-test, unknown)
#
# Designed to be read by wave workers confirming bead shape assignments.

set -uo pipefail

file="${1:?usage: suite-shape.sh <suite-file>}"

shapes=""

# cp-a-tree: copies the spira source tree to create a fixture installation
grep -qE 'cp -[ra].*\$HERE|cp -[ra].*spira/|SPIRA_HOME=\$TMP|cp.*\$HERE.*(lib|conf|suite|aeon|landing|sentinel|install)' "$file" 2>/dev/null \
  && shapes="${shapes:+$shapes }cp-a-tree"

# stub: creates fake binaries/scripts to replace real ones
grep -qE "printf.*'#!/|printf.*\"#!/|mkdir.*stub|mkdir.*shim|mkdir.*SHIM|cat\s*>.*\.sh|> \"\\\$TMP.*\.sh|> \\\$TMP.*\.sh|create_stub|stub_bd|SPIRA_BD=.*stub|SPIRA_PATH=.*stub" "$file" 2>/dev/null \
  && shapes="${shapes:+$shapes }stub"

# home-writer: suite sets HOME to a temp dir
grep -qE 'export HOME=\$TMP|HOME="\$TMP[^/]|^HOME=\$TMP|HOME=\$\(mktemp|env.*HOME=\$TMP|env -i HOME=' "$file" 2>/dev/null \
  && shapes="${shapes:+$shapes }home-writer"

# real-git: creates actual git repositories as test infrastructure
grep -qE 'git init|git clone|init_bare_remote|make_bare|bare_remote|\.git"' "$file" 2>/dev/null \
  && shapes="${shapes:+$shapes }real-git"

# testdb: uses testdb_up (a real dolt fixture)
grep -qE 'testdb_up|testdb\.sh' "$file" 2>/dev/null \
  && shapes="${shapes:+$shapes }testdb"

# real-systemd: calls real user systemd, not inside a container
grep -qE '^systemctl --user|^\s+systemctl --user|user@1001\.service' "$file" 2>/dev/null \
  && ! grep -qE 'testenv\.sh up|XDG_RUNTIME_DIR is not /run/user' "$file" 2>/dev/null \
  && shapes="${shapes:+$shapes }real-systemd"

# hermetic: suite declared itself safe to run without a container
grep -qE '^# hermetic-ok:' "$file" 2>/dev/null \
  && shapes="${shapes:+$shapes }hermetic"

# rust-test: covers loom or cockpit-panel rust source (cargo test, not bash)
grep -qE 'loom/src/|cockpit/panel/src/' <(grep '^# covers:' "$file" 2>/dev/null) 2>/dev/null \
  && shapes="${shapes:+$shapes }rust-test"

echo "${shapes:-unknown}"
