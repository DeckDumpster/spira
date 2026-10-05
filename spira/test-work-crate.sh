#!/usr/bin/env bash
#
# test-work-crate.sh — the `work` client crate's pure logic (cargo test -p work) and the
# built binary's own refusal/cannot-tell behavior with no server reachable.
#
# The refusal check (a verb naming any bead other than the bound one) is asserted here
# WITHOUT a reachable spira-lc socket, precisely to prove it never needed one: refused
# means the client decided before any network call, not that a request round-tripped and
# came back denied (law-fail-closed-at-the-source). The container-tier suite
# (test-work-container.sh) is what proves a request that clears this check reaches a real
# spira-lc and does what it says.
#
# THE RESTRICTED-ENVIRONMENT ISOLATION (no `bd` on PATH, no DB credential reachable) USED
# TO BE TESTED HERE, against the standalone `spira/work-env.sh` wrapper. That script is
# retired (sp-zpaq0, rewrite wave 5): its only caller was aeon.sh/aeon itself
# (work/DESIGN.md §2), so there is no standalone binary left to invoke. The property moved
# with the mechanism — `aeon/src/restrict.rs`'s own unit tests (`cargo test -p aeon`) hold
# the allow-list to account (bd/SPIRA_DB never leak, the bound bead id always does), and
# test-aeon-lifecycle-cutover.sh still proves it end to end against a real spira-lc.
#
# tier: T1
# covers: work/* spira-lc/src/work.rs spira-lc/src/bd.rs spira/conf.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin"

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# Same hazard as test-lifecycle-container.sh: a suite run inside testenv-batch.sh's own
# podman exec inherits its own CARGO_TARGET_DIR; pin one this suite controls.
CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
export CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD"
export CARGO_TERM_COLOR=never

out="$("$CARGO_BIN" test --manifest-path "$REPO/work/Cargo.toml" --quiet 2>&1)"
wantrc "cargo test -p work" 0 $?
want "cargo test -p work: all pass, none ignored" "test result: ok" "$out"
nowant "cargo test -p work: nothing failed" "FAILED" "$out"

# The tree's own `work`, resolved on the suite's PATH (sp-gypjk); its absolute path is kept
# because a case below runs it under a minimal `env -i PATH`.
WORK_BIN="$(command -v work)" || bail "work is not on PATH"

# ── no SPIRA_WORK_BEAD_ID: cannot tell, never a guess at which bead ──────────────────
out="$(env -i PATH="/usr/bin:/bin" "$WORK_BIN" show 2>&1)"; rc=$?
is   "no bound bead: exits 2 (cannot tell)" "2"                    "$rc"
want "no bound bead: names the missing var" "SPIRA_WORK_BEAD_ID"   "$out"

# ── the foreign-bead refusal happens before any socket connection ───────────────────
# POSITIVE CONTROL: the same call with the bound bead named instead reaches the (absent)
# socket and fails with "cannot tell", not "refused" — proving the refusal case below is
# a real, distinguishing decision and not just "everything fails without a socket".
out="$(env -i PATH="/usr/bin:/bin" SPIRA_WORK_BEAD_ID="sp-bound1" SPIRA_LC_SOCKET="$TMP/no-such-socket" \
    "$WORK_BIN" note "an ordinary note" 2>&1)"; rc=$?
is "POSITIVE CONTROL: bound bead, unreachable socket: exits 2 (cannot tell)" "2" "$rc"

out="$(env -i PATH="/usr/bin:/bin" SPIRA_WORK_BEAD_ID="sp-bound1" SPIRA_LC_SOCKET="$TMP/no-such-socket" \
    "$WORK_BIN" note "sp-other2" 2>&1)"; rc=$?
is   "foreign bead in note: refused (exit 3), never reaches the socket" "3"        "$rc"
want "foreign bead in note: names the offending id"                     "sp-other2" "$out"
want "foreign bead in note: names the bound bead"                       "sp-bound1" "$out"

# superseded-by names a different bead by design — must NOT be refused.
out="$(env -i PATH="/usr/bin:/bin" SPIRA_WORK_BEAD_ID="sp-bound1" SPIRA_LC_SOCKET="$TMP/no-such-socket" \
    "$WORK_BIN" superseded-by sp-other2 2>&1)"; rc=$?
is "superseded-by names a different bead: NOT refused (exits 2, cannot tell — reached for the socket)" "2" "$rc"

out="$(env -i PATH="/usr/bin:/bin" SPIRA_WORK_BEAD_ID="sp-bound1" \
    "$WORK_BIN" close 2>&1)"; rc=$?
is "unknown verb: refused (exit 3)" "3" "$rc"

# The restricted-environment isolation this section used to assert against the standalone
# work-env.sh wrapper moved with the mechanism to aeon/src/restrict.rs (sp-zpaq0) — see
# this file's header comment. Its own unit tests hold the allow-list to account; nothing
# stands alone here to invoke any more.

tl_summary
