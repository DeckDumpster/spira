#!/usr/bin/env bash
#
# test-sop.sh — sop: wiring smoke test over the real binary, a real scratch beads store,
# and the real lib.sh (via bdq/bdjson).
#
# sop.sh (bash) is retired (sp-8fsql); its decision tree — the shape validator, the ledger's
# three-valued read, the MATCH scoring, the METRIC downgrade, the closing-rule note text —
# is now `logic.rs` in the `sop` crate and is covered by 57 `cargo test -p sop` cases (see
# its DESIGN.md §5), replacing this suite's old ~110 assertions and the `SOP_SHELF_CMD`
# seam test-sop.sh used to drive them through. What THAT suite cannot cover — because it
# never touches a real subprocess, a real `bd`, or the real `lib.sh` seam — is what this
# suite checks instead:
#
#   1. the real binary really shells out through `lib.sh` (`bdq`/`bdjson`) to a real `bd`,
#      not a stub — write/show/list/match/applied/log/digest/retire/lint, end to end,
#      against a scratch store this suite creates and destroys (law-probe-a-fixture-not-
#      production).
#   2. `write` really refuses a shape violation and really refuses operator infrastructure
#      (a real `spira-lint --only inventory --scan` subprocess, not a fake one).
#   3. `synth` really resolves the worktree-root case via real `git` calls: writing from
#      inside a worktree of `SPIRA_WIKI` lands the page in the WORKTREE, never the shared
#      checkout every other worktree hangs off (the exact defect the bash's own header
#      warned about).
#   4. the chamber briefs and `aeon`'s rendered "Runbooks on the shelf" hint both name a
#      bare `sop`, not a deleted `sop.sh` path.
#
# tier: T1
# covers: sop/src/*.rs spira/chamber/ops.md spira/chamber/ops.fayth spira/chamber/concierge.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-sop.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-sop: cargo not found — sop binary cannot be built"
    exit 77
fi
BD_BIN="$(command -v bd 2>/dev/null || true)"
if [ -z "$BD_BIN" ]; then
    echo "SKIP test-sop: bd not found — cannot create a scratch store"
    exit 77
fi

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

CRATE_ROOT="$HERE/../sop"
BUILD_LOG="$T/cargo-build.log"
if ! CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$T/target" \
    "$CARGO_BIN" build --manifest-path "$CRATE_ROOT/Cargo.toml" -p sop >"$BUILD_LOG" 2>&1
then
    bad "cargo build -p sop" "see $BUILD_LOG"
    tail -60 "$BUILD_LOG" >&2
    tl_summary
fi
ok "cargo build -p sop"
SOP="$T/target/debug/sop"
[ -x "$SOP" ] || bail "sop binary not found at $SOP"

DB="$T/db"; mkdir -p "$DB"
( cd "$DB" && "$BD_BIN" init --prefix sptest >/dev/null 2>&1 )
[ -d "$DB/.beads" ] || bail "scratch beads store did not initialise at $DB"

RUN="$T/run"; mkdir -p "$RUN"
run_sop() {
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_DB="$DB" SPIRA_BD="$BD_BIN" SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" \
        "$SOP" "$@" 2>&1
}

GOOD='SYMPTOM: disk is full
CHECK: df -h
FIX: clear /tmp
'

echo
echo "1. write / show / list — the real lib.sh seam reaches a real bd"
out="$(printf '%s' "$GOOD" | run_sop write test-disk-full -)"; rc=$?
is "write exits 0" "0" "$rc"
want "write confirms the key" "wrote sop-test-disk-full" "$out"
out="$(run_sop show test-disk-full)"
want "show returns the stored text" "disk is full" "$out"
out="$(run_sop list)"
want "list names the key" "sop-test-disk-full" "$out"
want "list counts one SOP" "1 SOP(s) on the shelf" "$out"

echo
echo "2. write refuses a shape violation and operator infrastructure"
out="$(printf 'SYMPTOM: only this\n' | run_sop write test-bad -)"; rc=$?
is "missing-fields write exits 1" "1" "$rc"
want "names the missing field" "missing required field: CHECK" "$out"
# Built by concatenation, not as one literal string: inventory's own structural pattern
# (/home/[a-z]+/) would otherwise match THIS SOURCE FILE too, since inventory scans the
# whole repository, not just the SOP body a real `sop write` call sends it at runtime.
_leak_prefix="/home"; _leak_user="op"
out="$(printf 'SYMPTOM: x\nCHECK: y\nFIX: rm -rf %s/%s/whatever\n' "$_leak_prefix" "$_leak_user" | run_sop write test-leaky -)"; rc=$?
is "operator-path write exits 1" "1" "$rc"
want "refuses operator infrastructure" "operator infrastructure" "$out"
out="$(run_sop list)"
nowant "the refused SOPs were never stored" "test-bad" "$out"
nowant "the refused SOPs were never stored (leaky)" "test-leaky" "$out"

echo
echo "3. match scores the payload; validate checks shape with no shelf, no bd"
out="$(printf 'disk is full again' | run_sop match -)"
want "match falls back to key tokens" "key-tokens" "$out"
out="$(printf '%s' "$GOOD" | run_sop validate anything)"; rc=$?
is "validate of a clean SOP exits 0" "0" "$rc"
out="$(printf 'METRIC: bad shape\nSYMPTOM: x\nCHECK: y\nFIX: z\n' | run_sop validate anything)"; rc=$?
is "validate of a malformed METRIC exits 1" "1" "$rc"
want "names the METRIC problem" "METRIC must be KEY SUBCMD" "$out"

echo
echo "4. applied — a real ledger line and a real (failing, fake-bead) note attempt"
# A bead note write against a bead that does not exist in the store fails — and the bash
# original exits 1 in exactly this case ("the ledger line was written but the note ...
# was NOT"), so a real, non-fake bead would be required to see exit 0 here. The ledger
# line itself is still written either way (checked below), which is the load-bearing part.
out="$(run_sop applied test-disk-full --bead fake-bead-id --check pass --held yes)"; rc=$?
is "applied on an unwritable note still reports the bash's own exit 1" "1" "$rc"
want "names which bead's note did not land" "the note on fake-bead-id was NOT" "$out"
LEDGER="$RUN/sop/applied.jsonl"
[ -f "$LEDGER" ] || bad "the ledger file was created" "not found at $LEDGER"
line="$(tail -1 "$LEDGER" 2>/dev/null)"
want "the ledger line names the bead" '"bead":"fake-bead-id"' "$line"
want "the ledger line records held=yes" '"held":"yes"' "$line"
out="$(SPIRA_SOP_LEDGER="$LEDGER" run_sop log --bead fake-bead-id)"; rc=$?
is "log --bead finds the record" "0" "$rc"
want "log prints the recorded line" "test-disk-full" "$out"

echo
echo "5. digest and lint over the real shelf"
out="$(run_sop digest)"
want "digest names the key with a hash" "sop-test-disk-full " "$out"
out="$(run_sop lint)"; rc=$?
is "lint of a clean shelf exits 0" "0" "$rc"
want "lint reports all valid" "all valid" "$out"

echo
echo "6. retire removes it"
out="$(run_sop retire test-disk-full)"; rc=$?
is "retire exits 0" "0" "$rc"
out="$(run_sop list)"
nowant "the retired key is gone" "sop-test-disk-full" "$out"

echo
echo "7. synth resolves the WORKTREE root, never the shared checkout it hangs off"
printf '%s' "$GOOD" | run_sop write synth-target - >/dev/null
WIKI="$T/wiki"; mkdir -p "$WIKI/wiki/notes"
git -C "$WIKI" init -q
git -C "$WIKI" config user.email t@t; git -C "$WIKI" config user.name t
git -C "$WIKI" add -A; git -C "$WIKI" commit -qm init --allow-empty >/dev/null 2>&1
touch "$WIKI/wiki/notes/.gitkeep"; git -C "$WIKI" add -A; git -C "$WIKI" commit -qm keep >/dev/null 2>&1
git -C "$WIKI" worktree add "$T/wiki-wt" -b sop-test-wt -q
(
    cd "$T/wiki-wt"
    env -i PATH="$PATH" HOME="$HOME" SPIRA_DB="$DB" SPIRA_BD="$BD_BIN" SPIRA_HOME="$HERE" \
        SPIRA_RUN="$RUN" SPIRA_WIKI="$WIKI" "$SOP" synth
) >"$T/synth-out.log" 2>&1
rc=$?
is "synth exits 0 from inside the worktree" "0" "$rc"
if [ -f "$T/wiki-wt/wiki/notes/standard-operating-procedures.md" ]; then
    ok "synth wrote into the WORKTREE"
else
    bad "synth wrote into the WORKTREE" "not found at $T/wiki-wt/wiki/notes/standard-operating-procedures.md"
fi
if [ -e "$WIKI/wiki/notes/standard-operating-procedures.md" ]; then
    bad "synth did NOT write into the shared checkout" "found at $WIKI/wiki/notes/standard-operating-procedures.md"
else
    ok "synth did NOT write into the shared checkout"
fi

echo
echo "8. the chamber briefs and aeon's rendered hint name a bare sop, not a deleted sop.sh"
if grep -rqE '(^|[^.])\bsop\.sh\b' "$HERE/chamber/ops.md" "$HERE/chamber/ops.fayth" "$HERE/chamber/concierge.md" 2>/dev/null; then
    bad "no chamber brief names sop.sh" "$(grep -rnE '(^|[^.])\bsop\.sh\b' "$HERE/chamber/ops.md" "$HERE/chamber/ops.fayth" "$HERE/chamber/concierge.md" 2>/dev/null)"
else
    ok "no chamber brief names sop.sh"
fi
if [ -f "$HERE/sop.sh" ]; then
    bad "sop.sh is deleted" "still present at $HERE/sop.sh"
else
    ok "sop.sh is deleted"
fi

tl_summary
