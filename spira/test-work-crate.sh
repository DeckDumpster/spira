#!/usr/bin/env bash
#
# test-work-crate.sh — the `work` client crate's pure logic (cargo test -p work), the
# built binary's own refusal/cannot-tell behavior with no server reachable, and
# work-env.sh's isolation (no `bd` on PATH, no DB credential in the environment).
#
# The refusal check (a verb naming any bead other than the bound one) is asserted here
# WITHOUT a reachable spira-lc socket, precisely to prove it never needed one: refused
# means the client decided before any network call, not that a request round-tripped and
# came back denied (law-fail-closed-at-the-source). The container-tier suite
# (test-work-container.sh) is what proves a request that clears this check reaches a real
# spira-lc and does what it says.
#
# tier: T1
# covers: work/* spira-lc/src/work.rs spira-lc/src/bd.rs spira/work-env.sh spira/conf.sh
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

"$CARGO_BIN" build --manifest-path "$REPO/work/Cargo.toml" --quiet 2>"$TMP/build.log" \
    || bail "work failed to build: $(cat "$TMP/build.log")"
WORK_BIN="$CARGO_TARGET_DIR_FOR_BUILD/debug/work"
[ -x "$WORK_BIN" ] || bail "work binary missing after build: $WORK_BIN"

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

# ── work-env.sh: bd absent from PATH, no DB credential readable ─────────────────────
# POSITIVE CONTROL first: bd IS found on the ambient PATH, so its absence below is the
# wrapper's doing, not a fluke of this box's environment (law-absence-needs-a-positive-control).
if command -v bd >/dev/null 2>&1; then
    ok "POSITIVE CONTROL: bd is on the ambient PATH outside the wrapper"
else
    skip "bd is not installed on this box at all; the wrapper's exclusion cannot be distinguished from its absence"
fi

out="$(cd "$REPO" && SPIRA_WORK_BIN="$WORK_BIN" SPIRA_DB="/should/not/leak" SPIRA_BD="/should/not/leak" \
    SPIRA_LC_PASSWORD_FILE="/should/not/leak" \
    bash "$HERE/work-env.sh" sp-bound1 -- bash -c '
        command -v bd >/dev/null 2>&1 && { echo "BD_FOUND"; exit 1; }
        command -v work >/dev/null 2>&1 || { echo "WORK_MISSING"; exit 1; }
        [ "$SPIRA_WORK_BEAD_ID" = sp-bound1 ] || { echo "BEAD_ID_WRONG"; exit 1; }
        env | grep -qi "SPIRA_DB\|SPIRA_BD\|SPIRA_LC_PASSWORD\|CREDENTIAL\|_TOKEN\|_SECRET" && { echo "CREDENTIAL_LEAKED"; exit 1; }
        echo OK
    ' 2>&1)"; rc=$?
is   "work-env.sh: the wrapped command runs cleanly" "0"  "$rc"
want "work-env.sh: bd is absent from PATH inside it" "OK" "$out"
nowant "work-env.sh: bd was NOT found inside it"        "BD_FOUND"        "$out"
nowant "work-env.sh: no credential-shaped var leaked"   "CREDENTIAL_LEAKED" "$out"

tl_summary
