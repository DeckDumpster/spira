#!/usr/bin/env bash
#
# test-plan-matrix.sh — plan-matrix.sh's own positive control (item 3: A DERIVED COVERAGE
# MATRIX) and plan-matrix-fence.sh's gate-wiring confirmation.
#
# THE PROPERTY: docs/test-plan/coverage.json and COVERAGE.md are regenerated whole and never
# hand-edited (law-regenerate-derived-summaries); `--check` is what the gate runs, so a copy
# that no longer matches a fresh regeneration must fail it, and a freshly regenerated copy
# must pass it. Proven over a scratch repository so this suite never touches the real,
# 1400-line docs/test-plan/coverage.json.
#
# host-reason: reads suite source and scratch git repos only; no database, no systemd
#
# tier: T1
# covers: spira/plan-matrix.sh spira/plan-matrix-fence.sh spira/gate-touched.sh
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
# SEEN RED: no coverage.json/COVERAGE.md exist yet — --check must refuse,
# never silently report clean over a matrix it has never written.
# ==========================================================================
out="$(matrix --check)"; rc=$?
isnz "SEEN RED: --check refuses a missing coverage.json/COVERAGE.md" "$rc"
want "and names the file" "coverage.json" "$out"

# ==========================================================================
# Regenerate for real, then SEEN GREEN: --check passes against its own
# fresh output.
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

out="$(matrix --check)"; rc=$?
[ "$rc" = 0 ] && ok "SEEN GREEN: --check passes against a fresh regeneration" \
    || bad "SEEN GREEN: --check passes against a fresh regeneration" "$out"

# ==========================================================================
# SEEN RED again: hand-editing coverage.json makes it stale.
# ==========================================================================
printf '{"api_version":"stale","areas":[]}' > "$ROOT/docs/test-plan/coverage.json"
out="$(matrix --check)"; rc=$?
isnz "SEEN RED: a hand-edited coverage.json fails --check" "$rc"

# ==========================================================================
# GATE WIRING: gate-touched.sh calls plan-matrix-fence.sh, which calls both
# plan-lint.sh --orphans and plan-matrix.sh --check — confirmed by source
# reference, the same style test-build-fence.sh uses for build-fence.sh.
# ==========================================================================
gt="$(cat "$HERE/gate-touched.sh")"
want "gate-touched.sh calls plan-matrix-fence.sh" "plan-matrix-fence.sh" "$gt"

fence_src="$(cat "$HERE/plan-matrix-fence.sh")"
want "plan-matrix-fence.sh calls plan-lint.sh --orphans" "plan-lint.sh" "$fence_src"
want "plan-matrix-fence.sh calls plan-matrix.sh --check" "plan-matrix.sh" "$fence_src"

tl_summary
