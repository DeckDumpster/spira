#!/usr/bin/env bash
#
# test-plan-lint.sh — the test-plan lint's own fence: every suite declares its tier and UC
# coverage, every UC id it names exists in the typed catalogue, a gap in T0-T3 coverage is
# reported without failing the lint, and a suite deletion that orphans a use case's last
# cover is refused unless the catalogue marks it uncovered.
#
# THE POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control): a
# clean fixture proves nothing about a lint that never fires. Each failure
# mode is planted (SEEN RED) before the matching fix is shown to pass
# (SEEN GREEN).
#
# THE WHOLE-TREE WALK IS OVER A SCRATCH REPOSITORY — plan-lint.sh resolves
# its ROOT via git from its own location, so copying it (with suite-covers.sh,
# test-plan-bin.sh and suite-coverage-json.sh) into a throwaway git repo is
# enough to isolate every assertion from the real, not-yet-migrated spira/
# corpus. SPIRA_TEST_PLAN_BIN is exported to a binary built ONCE from the
# real repository's own test-plan/ crate — the scratch repo never needs
# cargo itself, it only needs somewhere to point.
#
# host-reason: reads suite source and scratch git repos only; no database, no systemd
#
# tier: T1
# covers: spira/plan-lint.sh spira/suite-covers.sh spira/plan-matrix-fence.sh spira/test-plan-bin.sh spira/suite-coverage-json.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REAL_ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
isz()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }

echo "test-plan-lint.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-plan-lint: cargo not found on PATH or at ~/.cargo/bin" >&2
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"
if ! "$CARGO_BIN" build --release --manifest-path "$REAL_ROOT/test-plan/Cargo.toml" >&2; then
    echo "FAIL building the real test-plan binary: cargo build failed" >&2
    exit 1
fi
# CARGO_TARGET_DIR, when set (the testenv container points it off the bind mount), is where
# the build above actually landed; only its absence means cargo used $REAL_ROOT/target.
export SPIRA_TEST_PLAN_BIN="${CARGO_TARGET_DIR:-$REAL_ROOT/target}/release/test-plan"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
ROOT="$TMP/root"
mkdir -p "$ROOT/spira" "$ROOT/docs/test-plan"
cp "$HERE/plan-lint.sh" "$ROOT/spira/plan-lint.sh"
cp "$HERE/suite-covers.sh" "$ROOT/spira/suite-covers.sh"
cp "$HERE/test-plan-bin.sh" "$ROOT/spira/test-plan-bin.sh"
cp "$HERE/suite-coverage-json.sh" "$ROOT/spira/suite-coverage-json.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

lint() { env -i PATH="$PATH" HOME="$TMP" TERM=dumb SPIRA_TEST_PLAN_BIN="$SPIRA_TEST_PLAN_BIN" \
    bash "$ROOT/spira/plan-lint.sh" "$@" 2>&1; }
commit() { git -C "$ROOT" add -A && git -C "$ROOT" commit -q -m "$1"; }

cat > "$ROOT/docs/test-plan/dispatch.toml" <<'EOF'
api_version = "test-plan/v1"
area = "dispatch"

[[use_case]]
id = "UC-dispatch-01"
tier = "T1"
statement = "a claim writes a lease"

[[use_case]]
id = "UC-dispatch-02"
tier = "T2"
statement = "a stale lease is reclaimed"

[[use_case]]
id = "UC-dispatch-05"
tier = "T2"
statement = "used only by the --orphans section below"
EOF

# A clean suite that stays in the tree so withdrawing a planted offender does
# not leave an empty corpus (empty corpus is its own, distinct exit code).
CLEAN="$ROOT/spira/test-planted-clean.sh"
printf '#!/usr/bin/env bash\n# tier: T0\n# covers: spira/lib.sh\necho clean\n' > "$CLEAN"
commit "seed"

PLANTED="$ROOT/spira/test-planted.sh"

# ==========================================================================
# POSITIVE CONTROL 1: missing headers. Plant a suite with neither # tier:
# nor # covers:; the lint must refuse it and name both. Withdraw it (SEEN
# GREEN) and require a clean pass.
# ==========================================================================
printf '#!/usr/bin/env bash\necho hello\n' > "$PLANTED"
out="$(lint)"; rc=$?
isnz "SEEN RED: missing headers are refused" "$rc"
want "and names the missing tier"   "missing # tier:"   "$out"
want "and names the missing covers" "missing # covers:" "$out"

rm "$PLANTED"
out="$(lint)"; rc=$?
isz "SEEN GREEN: withdrawing the planted suite clears the lint" "$rc"

# ==========================================================================
# POSITIVE CONTROL 2: unknown UC id. A # covers: line naming a UC id absent
# from every docs/test-plan/*.toml catalogue is refused.
# ==========================================================================
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/dispatch.sh UC-dispatch-99\necho hi\n' \
    > "$PLANTED"
out="$(lint)"; rc=$?
isnz "SEEN RED: unknown UC id is refused" "$rc"
want "and names the unknown id" "UC-dispatch-99" "$out"

rm "$PLANTED"
out="$(lint)"; rc=$?
isz "SEEN GREEN: withdrawing the unknown-UC suite clears the lint" "$rc"

# ==========================================================================
# A suite with both headers and a real UC id passes.
# ==========================================================================
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/dispatch.sh UC-dispatch-01\necho hi\n' \
    > "$PLANTED"
out="$(lint)"; rc=$?
isz "a suite with valid tier, covers and UC id passes" "$rc"
rm "$PLANTED"

# ==========================================================================
# --check <file> checks a single suite without walking the whole corpus.
# ==========================================================================
GOOD="$TMP/standalone-good.sh"
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/dispatch.sh UC-dispatch-02\necho hi\n' \
    > "$GOOD"
out="$(lint --check "$GOOD")"; rc=$?
isz "--check: a valid standalone suite passes" "$rc"

BADFILE="$TMP/standalone-bad.sh"
printf '#!/usr/bin/env bash\necho hi\n' > "$BADFILE"
out="$(lint --check "$BADFILE")"; rc=$?
isnz "--check: a standalone suite missing headers fails" "$rc"

# ==========================================================================
# --gaps: UC-dispatch-02 (T2) is declared but no suite in the corpus covers
# it — reported, but the lint's own exit code stays 0 (report, not fail).
# ==========================================================================
out="$(lint --gaps)"; rc=$?
isz "--gaps never fails the lint" "$rc"
want "--gaps reports the uncovered T2 use case" "UC-dispatch-02" "$out"

# Cover it, then the gap must clear.
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/dispatch.sh UC-dispatch-02\necho hi\n' \
    > "$ROOT/spira/test-covers-02.sh"
out="$(lint --gaps)"; rc=$?
isz "--gaps still exits 0 once covered" "$rc"
[[ "$out" != *"UC-dispatch-02"* ]] && ok "--gaps: covering a use case clears its gap" \
    || bad "--gaps: covering a use case clears its gap" "still reported: $out"
rm "$ROOT/spira/test-covers-02.sh"

# ==========================================================================
# An empty corpus refuses to report clean (law-absence-needs-a-positive-control).
# ==========================================================================
EMPTY_ROOT="$TMP/empty"; mkdir -p "$EMPTY_ROOT/spira" "$EMPTY_ROOT/docs/test-plan"
cp "$HERE/plan-lint.sh" "$EMPTY_ROOT/spira/plan-lint.sh"
cp "$HERE/suite-covers.sh" "$EMPTY_ROOT/spira/suite-covers.sh"
cp "$HERE/test-plan-bin.sh" "$EMPTY_ROOT/spira/test-plan-bin.sh"
cp "$HERE/suite-coverage-json.sh" "$EMPTY_ROOT/spira/suite-coverage-json.sh"
git init -q -b main "$EMPTY_ROOT"
git -C "$EMPTY_ROOT" config user.email t@t; git -C "$EMPTY_ROOT" config user.name t
rc_empty=0
env -i PATH="$PATH" HOME="$TMP" TERM=dumb SPIRA_TEST_PLAN_BIN="$SPIRA_TEST_PLAN_BIN" \
    bash "$EMPTY_ROOT/spira/plan-lint.sh" > /dev/null 2>&1 || rc_empty=$?
[ "$rc_empty" = 3 ] && ok "empty corpus refuses to report clean (exit 3)" \
    || bad "empty corpus refuses to report clean (exit 3)" "got exit $rc_empty"

# ==========================================================================
# --orphans <ref>: DELETION WRITES THE PLAN (item 2). A suite that was the
# last cover of a use case is deleted; the lint fails against the ref before
# the deletion, and passes again once the use case is marked uncovered.
# ==========================================================================
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/dispatch.sh UC-dispatch-05\necho hi\n' \
    > "$ROOT/spira/test-covers-05.sh"
commit "add the only cover of UC-dispatch-05"
before_ref="$(git -C "$ROOT" rev-parse HEAD)"

rm "$ROOT/spira/test-covers-05.sh"
commit "delete it, without marking the catalogue"

out="$(lint --orphans "$before_ref")"; rc=$?
isnz "SEEN RED: deleting the last cover orphans the use case" "$rc"
want "and names the orphaned id" "UC-dispatch-05" "$out"

# Fix path 1: mark the use case uncovered.
python3 - "$ROOT/docs/test-plan/dispatch.toml" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
text = text.replace(
    'statement = "used only by the --orphans section below"\n',
    'statement = "used only by the --orphans section below"\n\n'
    '[use_case.uncovered]\n'
    'reason = "suite retired"\n'
    'date = "2026-09-25"\n'
    'bead = "sp-6pmer"\n',
)
open(path, "w").write(text)
PY
commit "mark UC-dispatch-05 uncovered"

out="$(lint --orphans "$before_ref")"; rc=$?
isz "SEEN GREEN: marking the use case uncovered clears the orphan" "$rc"

tl_summary
