#!/usr/bin/env bash
#
# test-plan-matrix.sh — plan-matrix.sh's own positive control (item 3: A DERIVED COVERAGE
# MATRIX) and the gate-wiring confirmation (spira-lint's plan-matrix rule, sp-ufbkh).
#
# THE PROPERTY: docs/test-plan/coverage.json and COVERAGE.md are regenerated whole and never
# hand-edited (law-regenerate-derived-summaries), and untracked — a hand edit or a stale copy
# from a previous run is silently overwritten by the next regeneration rather than compared
# against, so there is nothing for two independently-regenerating branches to conflict or go
# stale over on merge (law-test-selection-and-plan-are-one-source). Proven over a scratch
# repository so this suite never touches the real, 1400-line docs/test-plan/coverage.json.
#
# host-reason: reads suite source and scratch git repos only; no database, no systemd
#
# tier: T1
# covers: spira/plan-matrix.sh spira-lint/src/rules/plan_matrix.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REAL_ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
isz()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }

echo "test-plan-matrix.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-plan-matrix: cargo not found on PATH or at ~/.cargo/bin" >&2
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
for f in plan-matrix.sh plan-lint.sh suite-covers.sh plan-bin.sh suite-coverage-json.sh tsd-timings-json.sh; do
    cp "$HERE/$f" "$ROOT/spira/$f"
done

cat > "$ROOT/docs/test-plan/dispatch.toml" <<'EOF'
api_version = "test-plan/v1"
area = "dispatch"

[[use_case]]
id = "UC-dispatch-01"
tier = "T1"
statement = "a claim writes a lease"
EOF
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/dispatch.sh UC-dispatch-01\necho hi\n' \
    > "$ROOT/spira/test-covers-01.sh"

matrix() { env -i PATH="$PATH" HOME="$TMP" TERM=dumb SPIRA_TEST_PLAN_BIN="$SPIRA_TEST_PLAN_BIN" \
    bash "$ROOT/spira/plan-matrix.sh" "$@" 2>&1; }

# ==========================================================================
# SEEN RED: a malformed catalogue must fail generation outright — plan-matrix.sh
# has nothing else to fall back on to detect a bad input.
# ==========================================================================
cp "$ROOT/docs/test-plan/dispatch.toml" "$TMP/dispatch.toml.good"
printf 'not valid toml{{{\n' > "$ROOT/docs/test-plan/dispatch.toml"
out="$(matrix)"; rc=$?
isnz "SEEN RED: a malformed catalogue fails matrix generation" "$rc"
cp "$TMP/dispatch.toml.good" "$ROOT/docs/test-plan/dispatch.toml"

# ==========================================================================
# SEEN GREEN: regeneration against the fixed catalogue writes both files.
# ==========================================================================
out="$(matrix)"; rc=$?
[ "$rc" = 0 ] && ok "plan-matrix.sh writes coverage.json and COVERAGE.md" \
    || bad "plan-matrix.sh writes coverage.json and COVERAGE.md" "$out"
[ -f "$ROOT/docs/test-plan/coverage.json" ] && ok "coverage.json exists" \
    || bad "coverage.json exists" "missing"
[ -f "$ROOT/docs/test-plan/COVERAGE.md" ] && ok "COVERAGE.md exists" \
    || bad "COVERAGE.md exists" "missing"
grep -q 'UC-dispatch-01' "$ROOT/docs/test-plan/COVERAGE.md" && \
    ok "COVERAGE.md names the use case" || bad "COVERAGE.md names the use case" "not found"

# ==========================================================================
# UNTRACKED, NEVER COMPARED: a hand-edited (or merge-stale) coverage.json is
# silently overwritten by the next regeneration, never diffed against — this
# is what keeps two independently-regenerating branches from ever conflicting
# or disagreeing on the file (law-test-selection-and-plan-are-one-source).
# ==========================================================================
printf '{"api_version":"stale","areas":[]}' > "$ROOT/docs/test-plan/coverage.json"
out="$(matrix)"; rc=$?
[ "$rc" = 0 ] && ok "regeneration succeeds over a hand-edited coverage.json" \
    || bad "regeneration succeeds over a hand-edited coverage.json" "$out"
grep -q 'UC-dispatch-01' "$ROOT/docs/test-plan/coverage.json" && \
    ok "the hand edit is gone — overwritten, not merged with" \
    || bad "the hand edit is gone — overwritten, not merged with" "$(cat "$ROOT/docs/test-plan/coverage.json")"

# ==========================================================================
# UNTRACKED BY GIT: coverage.json/COVERAGE.md must never be a committed file
# a merge of two branches could conflict over or leave stale.
# ==========================================================================
gi="$(cat "$REAL_ROOT/.gitignore" 2>/dev/null)"
want ".gitignore excludes coverage.json" "docs/test-plan/coverage.json" "$gi"
want ".gitignore excludes COVERAGE.md" "docs/test-plan/COVERAGE.md" "$gi"

# ==========================================================================
# GATE WIRING (sp-ufbkh): the gate runs the test plan's fence as spira-lint's plan-matrix
# rule, in the gate tree against SPIRA_GATE_BASE. gate-touched.sh ran plan-matrix-fence.sh
# behind a SPIRA_GATE_REPO check that never matched at the gate, so it never ran there;
# both scripts are gone (the selector is the suite-select binary, sp-wx2tw). Confirmed by
# source reference.
# ==========================================================================
rule_src="$(cat "$HERE/../spira-lint/src/rules/plan_matrix.rs" 2>/dev/null)"
want "spira-lint carries the plan-matrix rule" 'const NAME: &str = "plan-matrix"' "$rule_src"
want "the rule judges orphans against the base" "orphan_violations" "$rule_src"
want "the rule builds the matrix" "build_matrix" "$rule_src"

tl_summary
