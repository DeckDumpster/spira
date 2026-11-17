#!/usr/bin/env bash
#
# test-repo-contract-scaffold.sh — `repo-contract scaffold` on each registered product repo,
# cut as a detached worktree from its landing ref: the scaffold's flake enters, `check` reports
# the environment and deploy clauses met, and the toolchain the repo pins is the one the shell
# runs. The real checkouts are read only; their `git status --porcelain` must not change.
#
# tier: T4
# bare-host: needs the host nix and the registered checkouts; the corpus container has neither
# covers: repo-contract/** spira/test-repo-contract-scaffold.sh
set -uo pipefail
OPERATOR_TOML="${SPIRA_TOML:-}"
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-repo-contract-scaffold.sh"

BIN="$(command -v repo-contract || true)"
[ -n "$BIN" ] || bail "repo-contract is not on PATH"
[ -n "${SPIRA_NIX_PROFILE_BIN:-}" ] && [ -x "$SPIRA_NIX_PROFILE_BIN/nix" ] || skip "SPIRA_NIX_PROFILE_BIN names no nix"
NIX="$SPIRA_NIX_PROFILE_BIN/nix"
ROOTS="${SCAFFOLD_REPOS:-$(SPIRA_TOML="$OPERATOR_TOML" spira-config repo names 2>/dev/null | tr '\n' ' ')}"
[ -n "$ROOTS" ] || skip "SCAFFOLD_REPOS names no registered repository"
LIMIT="${SCAFFOLD_TIMEOUT:-1800}"
tl_config SPIRA_TEST_CASE_DEADLINE=$((LIMIT + 60))

TMP="$(mktemp -d)"
trap 'for t in "$TMP"/wt-*; do [ -d "$t" ] && git -C "$(cat "$t.root")" worktree remove --force "$t" 2>/dev/null; done; rm -rf "$TMP"' EXIT INT TERM

rc() { env -i HOME="$HOME" SPIRA_TOML="$OPERATOR_TOML" PATH="$PATH" SPIRA_RELEASE="${SCAFFOLD_RELEASE:-${SPIRA_RELEASE:-}}" SPIRA_NIX_PROFILE_BIN="$SPIRA_NIX_PROFILE_BIN" timeout "$LIMIT" "$BIN" "$@"; }

for repo in $ROOTS; do
    dir="$(SPIRA_TOML="$OPERATOR_TOML" SPIRA_RELEASE="${SPIRA_RELEASE:-}" spira-config repo root "$repo" 2>/dev/null || true)"
    [ -d "$dir/.git" ] || { bad "$repo: has no checkout in the registry" "root=$dir"; continue; }
    before="$(git -C "$dir" status --porcelain)"
    wt="$TMP/wt-$repo"
    echo "$dir" >"$wt.root"
    land="$(SPIRA_TOML="$OPERATOR_TOML" SPIRA_RELEASE="${SPIRA_RELEASE:-}" spira-config repo landref "$repo" 2>/dev/null)"
    git -C "$dir" worktree add -q --detach "$wt" "$land" || { bad "$repo: cut a worktree"; continue; }

    out="$(rc scaffold "$repo" --into "$dir" 2>&1)"; code=$?
    is "$repo: into the registered checkout is refused" 2 "$code"
    want "$repo: the refusal names its exit" "--into <a worktree>" "$out"

    out="$(rc scaffold "$repo" --into "$wt" 2>&1)"; code=$?
    is "$repo: scaffold exits 0 ($out)" 0 "$code"
    want "$repo: scaffold prints the flake it wrote" "flake.nix" "$out"

    out="$(rc check "$repo" --tree "$wt" --clauses environment,deploy 2>&1)"; code=$?
    is "$repo: environment and deploy are met ($out)" 0 "$code"
    want "$repo: environment met" "met environment" "$out"
    want "$repo: deploy met" "met deploy" "$out"

    rows="$(sed -n 's/^run *= *"\(.*\)"/\1/p' "$wt/.spira/checks.toml" 2>/dev/null)"
    [ -f "$wt/rust-toolchain.toml" ] && rows="cargo --version"$'\n'"$rows"
    out=""; code=0
    while IFS= read -r row; do
        [ -n "$row" ] || continue
        o="$(cd "$wt" && timeout "$LIMIT" "$NIX" develop "$wt" -c bash -c "$row" 2>&1)" || code=$?
        out="$out$o"$'\n'
    done <<<"$rows"
    is "$repo: every check row runs in the shell" 0 "$code"
    [ ! -f "$wt/rust-toolchain.toml" ] || want "$repo: the pinned toolchain is the one the shell runs" "$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$wt/rust-toolchain.toml")" "$out"

    is "$repo: the real checkout is untouched" "$before" "$(git -C "$dir" status --porcelain)"
done
tl_summary
