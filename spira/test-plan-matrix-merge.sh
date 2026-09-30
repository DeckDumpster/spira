#!/usr/bin/env bash
#
# test-plan-matrix-merge.sh — a merge of two branches that each add their own covered suite
# must never produce a stale coverage.json/COVERAGE.md fence failure
# (law-test-selection-and-plan-are-one-source).
#
# Builds a scratch git repo carrying copies of the real plan-matrix.sh, plan-lint.sh
# and friends, plus this checkout's own .gitignore. Branches it twice — each branch
# adds one new covered suite and use case in files the other branch never touches — merges
# both, and checks the merged tree passes the plan checks (plan-lint.sh --orphans and
# plan-matrix.sh, what the retired plan-matrix-fence.sh ran) and the selector (suite-select,
# sp-wx2tw) selects both new suites. Only docs/test-plan/coverage.json and COVERAGE.md are regenerated (and, before the
# fix, committed) by both branches, so they are the only place a merge can go stale. Proven
# over a scratch repository so this suite never touches the real docs/test-plan tree.
#
# host-reason: reads suite source and scratch git repos only; no database, no systemd
#
# tier: T1
# covers: spira/plan-matrix.sh spira/plan-lint.sh suite-select/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REAL_ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-plan-matrix-merge.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-plan-matrix-merge: cargo not found on PATH or at ~/.cargo/bin" >&2
    exit 77
fi
export PATH="$(dirname "$CARGO_BIN"):$PATH"
if ! "$CARGO_BIN" build --release --manifest-path "$REAL_ROOT/test-plan/Cargo.toml" >&2; then
    echo "FAIL building the real test-plan binary: cargo build failed" >&2
    exit 1
fi
export SPIRA_TEST_PLAN_BIN="${CARGO_TARGET_DIR:-$REAL_ROOT/target}/release/test-plan"
if ! "$CARGO_BIN" build --release --manifest-path "$REAL_ROOT/suite-select/Cargo.toml" >&2; then
    echo "FAIL building the real suite-select binary: cargo build failed" >&2
    exit 1
fi
SELECT_BIN="${CARGO_TARGET_DIR:-$REAL_ROOT/target}/release/suite-select"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
ROOT="$TMP/root"
mkdir -p "$ROOT/spira" "$ROOT/docs/test-plan"
for f in plan-matrix.sh plan-lint.sh suite-covers.sh plan-bin.sh \
         suite-coverage-json.sh tsd-timings-json.sh testlib.sh; do
    cp "$HERE/$f" "$ROOT/spira/$f"
done
# Carries THIS repo's own tracking rule for the generated files — the fixture proves the
# merge is clean (or not) under the rule actually in force, never a rule hand-picked here.
cp "$REAL_ROOT/.gitignore" "$ROOT/.gitignore"

git -C "$ROOT" init -q -b base
git -C "$ROOT" config user.email test@example.com
git -C "$ROOT" config user.name test

cat > "$ROOT/docs/test-plan/dispatch.toml" <<'EOF'
api_version = "test-plan/v1"
area = "dispatch"

[[use_case]]
id = "UC-dispatch-01"
tier = "T1"
statement = "a claim writes a lease"
EOF
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/dispatch.sh UC-dispatch-01\necho hi\n' \
    > "$ROOT/spira/test-covers-00.sh"
# A catch-all claiming every suite file itself, mirroring the real corpus's
# test-citations.sh — without it, the selector would refuse ANY new suite file as an
# unclaimed source file (spira/*.sh), a real but unrelated property of the selector this
# fixture must not trip over.
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/test-*.sh\necho catchall\n' \
    > "$ROOT/spira/test-catchall.sh"

matrix() { ( cd "$ROOT" && SPIRA_TEST_PLAN_BIN="$SPIRA_TEST_PLAN_BIN" bash spira/plan-matrix.sh "$@" ); }
fence()  { ( cd "$ROOT" && export SPIRA_TEST_PLAN_BIN="$SPIRA_TEST_PLAN_BIN" \
               && bash spira/plan-lint.sh --orphans "$1" && bash spira/plan-matrix.sh ); }

matrix >&2
git -C "$ROOT" add -A
git -C "$ROOT" commit -q -m base
BASE_SHA="$(git -C "$ROOT" rev-parse HEAD)"

# Branch A: a new covered suite for a new use case, in its own new catalogue file — the
# catalogue and suite files themselves never conflict; only the generated matrix can.
git -C "$ROOT" checkout -q -b branch-a
cat > "$ROOT/docs/test-plan/area-a.toml" <<'EOF'
api_version = "test-plan/v1"
area = "area-a"

[[use_case]]
id = "UC-area-a-01"
tier = "T1"
statement = "feature A works"
EOF
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/feature-a.sh UC-area-a-01\necho a\n' \
    > "$ROOT/spira/test-covers-a.sh"
: > "$ROOT/spira/feature-a.sh"
matrix >&2
git -C "$ROOT" add -A
git -C "$ROOT" commit -q -m "branch A: add suite+UC A"

# Branch B: same shape, off base, entirely disjoint files.
git -C "$ROOT" checkout -q -b branch-b "$BASE_SHA"
cat > "$ROOT/docs/test-plan/area-b.toml" <<'EOF'
api_version = "test-plan/v1"
area = "area-b"

[[use_case]]
id = "UC-area-b-01"
tier = "T1"
statement = "feature B works"
EOF
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/feature-b.sh UC-area-b-01\necho b\n' \
    > "$ROOT/spira/test-covers-b.sh"
: > "$ROOT/spira/feature-b.sh"
matrix >&2
git -C "$ROOT" add -A
git -C "$ROOT" commit -q -m "branch B: add suite+UC B"

# Merge B into A. Both branches independently regenerated coverage.json/COVERAGE.md; a repo
# that commits them either conflicts there, or merges silently onto content a fresh
# regeneration no longer matches. Either way this is the only place a real conflict can
# appear — the catalogues and suite files are disjoint by construction.
git -C "$ROOT" checkout -q branch-a
if ! git -C "$ROOT" merge -q --no-edit branch-b >"$TMP/merge.log" 2>&1; then
    conflicted="$(git -C "$ROOT" diff --name-only --diff-filter=U)"
    for cf in $conflicted; do
        case "$cf" in
            docs/test-plan/coverage.json|docs/test-plan/COVERAGE.md)
                # The realistic resolution rounds 92/93/95 hit before the fix: keep one
                # side rather than regenerate by hand.
                git -C "$ROOT" checkout --ours -- "$cf"
                git -C "$ROOT" add "$cf" ;;
            *)
                bad "merge produced an unexpected conflict in $cf" "$(cat "$TMP/merge.log")" ;;
        esac
    done
    git -C "$ROOT" commit -q -m "merge branch B (naive conflict resolution)"
fi
MERGED_SHA="$(git -C "$ROOT" rev-parse HEAD)"

out="$(fence "$BASE_SHA" 2>&1)"; rc=$?
wantrc "the plan checks pass on the merged tree" 0 "$rc"
[ "$rc" = 0 ] || printf '# fence output:\n%s\n' "$out" | sed 's/^/# /' >&2

sel="$(cd "$ROOT" && "$SELECT_BIN" select --base "$BASE_SHA" --head "$MERGED_SHA" \
    --repo "$ROOT" --suite-dir "$ROOT/spira" 2>"$TMP/select.log")"
want "the selector selects branch A's new suite" "test-covers-a.sh" "$sel"
want "the selector selects branch B's new suite" "test-covers-b.sh" "$sel"

tl_summary
