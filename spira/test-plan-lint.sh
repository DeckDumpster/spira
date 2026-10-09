#!/usr/bin/env bash
#
# test-plan-lint.sh — the test-plan lint's own fence: every suite declares its tier and UC
# coverage, every UC id it names exists in the typed catalogue, a T0-T3 use case with no cover
# and no uncovered marker fails the lint, and a suite deletion that orphans a use case's last
# cover is refused unless the catalogue marks it uncovered.
#
# THE POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control): a
# clean fixture proves nothing about a lint that never fires. Each failure
# mode is planted (SEEN RED) before the matching fix is shown to pass
# (SEEN GREEN).
#
# THE WHOLE-TREE WALK IS OVER A SCRATCH REPOSITORY — plan-lint.sh resolves
# its ROOT via git from its own location, so copying it (with suite-coverage-json.sh)
# into a throwaway git repo is enough to isolate every assertion from the real,
# not-yet-migrated spira/ corpus. test-plan and suite-select are the real tree's own
# binaries, by name on the suite's PATH (plan-lint.sh calls `suite-select header ...`
# for its tier/covers/UC reads since wave 4.36, sp-bobsp).
#
# host-reason: reads suite source and scratch git repos only; no database, no systemd
#
# tier: T1
# covers: spira/plan-lint.sh spira/suite-coverage-json.sh suite-select/ test-plan/ docs/test-plan/*.toml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REAL_ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
isz()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }

echo "test-plan-lint.sh"

# test-plan (and suite-select) are the tree's own, by name on the suite's PATH (sp-gypjk).
for _t in test-plan suite-select; do
    command -v "$_t" >/dev/null 2>&1 || bail "$_t is not on PATH"
done

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
ROOT="$TMP/root"
mkdir -p "$ROOT/spira" "$ROOT/docs/test-plan"
cp "$HERE/plan-lint.sh" "$ROOT/spira/plan-lint.sh"
cp "$HERE/suite-covers.sh" "$ROOT/spira/suite-covers.sh"
cp "$HERE/suite-coverage-json.sh" "$ROOT/spira/suite-coverage-json.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

lint() { env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
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
tier = "T4"
statement = "used only by the --orphans section below"
EOF

# A clean suite that stays in the tree so withdrawing a planted offender does
# not leave an empty corpus (empty corpus is its own, distinct exit code).
CLEAN="$ROOT/spira/test-planted-clean.sh"
printf '#!/usr/bin/env bash\n# tier: T0\n# covers: spira/lib.sh UC-dispatch-01 UC-dispatch-02\necho clean\n' > "$CLEAN"
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
# --gaps: a T2 use case no suite covers and no marker explains fails the lint; the marker
# clears it; a cover clears it.
# ==========================================================================
cp "$ROOT/docs/test-plan/dispatch.toml" "$TMP/dispatch.toml.orig"
printf '\n[[use_case]]\nid = "UC-dispatch-06"\ntier = "T2"\nstatement = "planted gap"\n' \
    >> "$ROOT/docs/test-plan/dispatch.toml"
out="$(lint --gaps)"; rc=$?
isnz "SEEN RED: --gaps fails on an uncovered T2 use case" "$rc"
want "--gaps reports the uncovered T2 use case" "UC-dispatch-06" "$out"
out="$(lint)"; rc=$?
isnz "SEEN RED: the default lint fails on an uncovered T2 use case" "$rc"
want "and names it" "UC-dispatch-06" "$out"

printf '\n[use_case.uncovered]\nreason = "r"\ndate = "2026-10-08"\nbead = "sp-x"\n' >> "$ROOT/docs/test-plan/dispatch.toml"
out="$(lint --gaps)"; rc=$?
isz "SEEN GREEN: an uncovered marker clears the gap" "$rc"
out="$(lint)"; rc=$?
isz "SEEN GREEN: and the default lint passes" "$rc"

cp "$TMP/dispatch.toml.orig" "$ROOT/docs/test-plan/dispatch.toml"
printf '\n[[use_case]]\nid = "UC-dispatch-06"\ntier = "T2"\nstatement = "planted gap"\n' \
    >> "$ROOT/docs/test-plan/dispatch.toml"
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/dispatch.sh UC-dispatch-06\necho hi\n' \
    > "$ROOT/spira/test-covers-06.sh"
out="$(lint --gaps)"; rc=$?
isz "SEEN GREEN: covering a use case clears its gap" "$rc"
rm "$ROOT/spira/test-covers-06.sh"
cp "$TMP/dispatch.toml.orig" "$ROOT/docs/test-plan/dispatch.toml"

# ==========================================================================
# LAUNCHERS: a use case with a launcher table is a gap until a suite covers it, whatever
# its uncovered marker says; a launcher whose site lost its needle fails the lint.
# ==========================================================================
cat > "$ROOT/docs/test-plan/launch.toml" <<'EOF'
api_version = "test-plan/v1"
area = "launch"

[[use_case]]
id = "UC-launch-01"
tier = "T2"
statement = "the registered status command starts"
launcher = { site = "somewhere/registers.rs", needle = "status_cmd" }

[use_case.uncovered]
reason = "not yet"
date = "2026-10-06"
bead = "sp-x"
EOF
mkdir -p "$ROOT/somewhere"
echo 'fn status_cmd() {}' > "$ROOT/somewhere/registers.rs"
out="$(lint --gaps)"; rc=$?
isz "--gaps with a launcher still exits 0 (launcher gaps are reported)" "$rc"
want "an uncovered launcher is reported even with an uncovered marker" "launcher gap: UC-launch-01" "$out"
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/x.sh UC-launch-01\necho hi\n' > "$ROOT/spira/test-launch.sh"
out="$(lint --gaps)"
[[ "$out" != *"launcher gap"* ]] && ok "covering the launcher clears its gap" \
    || bad "covering the launcher clears its gap" "still reported: $out"
echo 'fn renamed() {}' > "$ROOT/somewhere/registers.rs"
out="$(lint)"; rc=$?
isnz "SEEN RED: a launcher site that lost its needle fails the lint" "$rc"
want "and names the launcher" "UC-launch-01" "$out"
rm -rf "$ROOT/docs/test-plan/launch.toml" "$ROOT/somewhere" "$ROOT/spira/test-launch.sh"

# ==========================================================================
# An empty corpus refuses to report clean (law-absence-needs-a-positive-control).
# ==========================================================================
EMPTY_ROOT="$TMP/empty"; mkdir -p "$EMPTY_ROOT/spira" "$EMPTY_ROOT/docs/test-plan"
cp "$HERE/plan-lint.sh" "$EMPTY_ROOT/spira/plan-lint.sh"
cp "$HERE/suite-covers.sh" "$EMPTY_ROOT/spira/suite-covers.sh"
cp "$HERE/suite-coverage-json.sh" "$EMPTY_ROOT/spira/suite-coverage-json.sh"
git init -q -b main "$EMPTY_ROOT"
git -C "$EMPTY_ROOT" config user.email t@t; git -C "$EMPTY_ROOT" config user.name t
rc_empty=0
env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
    bash "$EMPTY_ROOT/spira/plan-lint.sh" > /dev/null 2>&1 || rc_empty=$?
[ "$rc_empty" = 3 ] && ok "empty corpus refuses to report clean (exit 3)" \
    || bad "empty corpus refuses to report clean (exit 3)" "got exit $rc_empty"

# ==========================================================================
# --orphans <ref>: DELETION WRITES THE PLAN (item 2). A suite that was the
# last cover of a use case is deleted; the lint fails against the ref before
# the deletion, and passes again once the use case is marked uncovered.
#
# AN UNRELATED SUITE NAMING A UC ID NO CATALOGUE KNOWS (the everyday state of
# every not-yet-migrated area on the real tree) STAYS IN THE CORPUS THROUGHOUT
# THIS WHOLE SECTION. --orphans must never trip on it: that whole-corpus
# unknown-UC check is `validate`'s and item 1's (sp-94lbj), deliberately not
# wired into the gate yet (spira-lint's plan-matrix rule runs --orphans only) — a version that
# ran it here would fail --orphans, and so the gate, on every branch today.
# ==========================================================================
printf '#!/usr/bin/env bash\n# tier: T1\n# covers: spira/unmigrated.sh UC-unmigrated-area-01\necho hi\n' \
    > "$ROOT/spira/test-unmigrated-area.sh"
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/dispatch.sh UC-dispatch-05\necho hi\n' \
    > "$ROOT/spira/test-covers-05.sh"
commit "add the only cover of UC-dispatch-05, plus an unrelated unmigrated-area suite"
before_ref="$(git -C "$ROOT" rev-parse HEAD)"

rm "$ROOT/spira/test-covers-05.sh"
commit "delete it, without marking the catalogue"

out="$(lint --orphans "$before_ref")"; rc=$?
isnz "SEEN RED: deleting the last cover orphans the use case" "$rc"
want "and names the orphaned id" "UC-dispatch-05" "$out"
[[ "$out" != *"UC-unmigrated-area-01"* ]] && \
    ok "--orphans never flags the unrelated unmigrated-area suite's unknown UC id" || \
    bad "--orphans never flags the unrelated unmigrated-area suite's unknown UC id" "$out"

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

# ==========================================================================
# A Rust test's `// covers: UC-…` comment is cover: the suite that covered the id can be
# deleted with no marker, and removing the annotation orphans it again.
# ==========================================================================
printf '#!/usr/bin/env bash\n# tier: T2\n# covers: spira/dispatch.sh UC-dispatch-02\necho hi\n' \
    > "$ROOT/spira/test-covers-02.sh"
commit "a suite covering UC-dispatch-02"
before_rs="$(git -C "$ROOT" rev-parse HEAD)"
mkdir -p "$ROOT/somecrate/src"
printf '#[test]\n// covers: UC-dispatch-02\nfn t() {}\n' > "$ROOT/somecrate/src/lib.rs"
rm "$ROOT/spira/test-covers-02.sh"
commit "migrate it into a Rust test"
out="$(lint --orphans "$before_rs")"; rc=$?
isz "a Rust '// covers:' annotation keeps the use case covered" "$rc"

printf '#[test]\nfn t() {}\n' > "$ROOT/somecrate/src/lib.rs"
commit "drop the annotation"
out="$(lint --orphans "$before_rs")"; rc=$?
isnz "SEEN RED: without the annotation the use case is orphaned" "$rc"
want "and names the orphaned id" "UC-dispatch-02" "$out"

tl_summary
