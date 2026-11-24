#!/usr/bin/env bash
# usage: lint-staged.sh <spira-lint> [args...]
#        lint-staged.sh --tree [args...]   (builds this tree's spira-lint first)
# Runs spira-lint under the tree's fixture config, as the round does, never under the caller's SPIRA_TOML.
# The round also layers its own host keys (repo paths); none of them is a lint rule's mode.
set -uo pipefail
tree="$PWD"
if [ "${1:-}" = --tree ]; then
    shift
    cargo build -q --config profile.dev.incremental=false -p spira-lint --bin spira-lint || exit 75
    set -- "${CARGO_TARGET_DIR:-$tree/target}/debug/spira-lint" "$@"
fi
_t="$(SPIRA_TOML="$tree/spira/testlib/complete.toml" release config-stage "$tree")" || { echo 'lint-staged: cannot stage the tree config delta' >&2; exit 75; }
rc=0
SPIRA_TOML="$_t" "$@" || rc=$?
case "$_t" in */release-config-*/layer*) rm -rf "${_t%%/layer*}";; esac
exit "$rc"
