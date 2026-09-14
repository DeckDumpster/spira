#!/usr/bin/env bash
#
# test-skew-prod-tree.sh — skew.sh check audits the trees systemd executes, not the git checkout.
#
# CLASS: sp-skew-audits-wrong-tree
#
# WHAT THIS SUITE IS FOR
# ----------------------
# skew.sh used to compare only $SPIRA_REPO (the git checkout) against origin/main. In
# split-checkout mode the release tree has no .git and was silently skipped. The drift
# detector was pointed at the one tree that cannot drift, so it reported clean while the
# executing tree fell further behind.
#
# The fix reads ExecStart= paths from installed unit files, resolves each to its harness
# root, and compares content against origin/main via git hash-object — which works without
# a .git directory in the executing tree.
#
# THE POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). Before claiming
# that a stale exec tree is detected, plant the offender and require detection. A check
# that never fires would pass the subsequent "silence when current" assertion too.
#
# THE FIXTURE IS A REAL GIT CLONE with a bare origin so `git fetch` works locally. The
# exec tree is a plain directory (no .git) representing a frozen release snapshot. A
# fixture unit file points ExecStart= into the exec tree; SPIRA_UNIT_DIR overrides the
# default unit directory so the test does not write to the operator's real service manager.
#
# covers: spira/skew.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-skew-prod-tree.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Build a bare origin and a git checkout (SPIRA_REPO).
# ---------------------------------------------------------------------------
ORIGIN="$TMP/origin"
git init --bare -q "$ORIGIN"

SRC="$TMP/src"
git init -q "$SRC"
git -C "$SRC" config user.email "test@test"
git -C "$SRC" config user.name "test"
mkdir -p "$SRC/spira" "$SRC/systemd"

# Harness signature files: scope_from_paths requires all three in the same dir.
printf '# harness boundary\n'  > "$SRC/spira/boundary"
printf '#!/usr/bin/env bash\n' > "$SRC/spira/gate.sh"
printf '#!/usr/bin/env bash\n# original content\n' > "$SRC/spira/lib.sh"

# Stub install.sh exits 0 for --diff so the STALE check produces no CANNOT-DIFF finding,
# which would otherwise suppress EXEC-TREE-STALE by setting hard=0.
cat > "$SRC/systemd/install.sh" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = "--diff" ] && { echo "stub: installed units match"; exit 0; }
exit 0
EOF
chmod +x "$SRC/systemd/install.sh"

git -C "$SRC" add spira/ systemd/
git -C "$SRC" commit -q -m "fixture: base commit"

DEF_BRANCH="$(git -C "$SRC" branch --show-current 2>/dev/null || echo master)"

# Push base to origin and clone.
git -C "$SRC" remote add origin "file://$ORIGIN"
git -C "$SRC" push -q origin "${DEF_BRANCH}:${DEF_BRANCH}"

SPIRA_REPO="$TMP/checkout"
git clone -q "file://$ORIGIN" "$SPIRA_REPO"
git -C "$SPIRA_REPO" config user.email "test@test"
git -C "$SPIRA_REPO" config user.name "test"

# ---------------------------------------------------------------------------
# Build the exec tree from the base commit — a plain directory snapshot with no .git.
# This simulates a frozen release tree extracted to disk.
# ---------------------------------------------------------------------------
EXEC_TREE="$TMP/exec-tree"
mkdir -p "$EXEC_TREE/spira" "$EXEC_TREE/systemd"
cp "$SRC/spira/boundary"         "$EXEC_TREE/spira/boundary"
cp "$SRC/spira/gate.sh"          "$EXEC_TREE/spira/gate.sh"
cp "$SRC/spira/lib.sh"           "$EXEC_TREE/spira/lib.sh"
cp "$SRC/systemd/install.sh"     "$EXEC_TREE/systemd/install.sh"

# ---------------------------------------------------------------------------
# Advance origin/main by adding a new file (simulates a landed commit).
# SPIRA_REPO fast-forwards to this commit; exec tree stays at the base.
# ---------------------------------------------------------------------------
printf '#!/usr/bin/env bash\n# landed fix — absent from the exec tree\n' \
    > "$SRC/spira/landed.sh"
git -C "$SRC" add spira/landed.sh
git -C "$SRC" commit -q -m "fixture: advance — add landed.sh"
git -C "$SRC" push -q origin "HEAD:${DEF_BRANCH}"

# Bring SPIRA_REPO current so that only the exec tree is stale, not SPIRA_REPO.
git -C "$SPIRA_REPO" fetch -q
git -C "$SPIRA_REPO" merge --ff-only -q "origin/${DEF_BRANCH}"

# ---------------------------------------------------------------------------
# Fixture unit files: ExecStart= points into the exec tree.
# SPIRA_UNIT_DIR replaces the real systemd user unit directory.
# ---------------------------------------------------------------------------
UNIT_DIR="$TMP/units"
mkdir -p "$UNIT_DIR"

cat > "$UNIT_DIR/spira-sentinel-prod.service" <<UNIT
[Service]
ExecStart=$EXEC_TREE/spira/gate.sh
UNIT

# ---------------------------------------------------------------------------
# run_skew <env-var=val>... — runs skew.sh check in a minimal isolated environment.
# ---------------------------------------------------------------------------
run_skew() {
    local run_dir; run_dir="$(mktemp -d "$TMP/run-XXXXX")"
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$SPIRA_REPO/spira" \
        SPIRA_REPO="$SPIRA_REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_UNIT_DIR="$UNIT_DIR" \
        "${@}" \
        bash "$HERE/skew.sh" check 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "positive control — stale exec tree is detected:"
# ===========================================================================
# The exec tree is at the base commit (no landed.sh); SPIRA_REPO is current.
# skew.sh must report EXEC-TREE-STALE and name the tree path and the missing file.
stale_out="$(run_skew)"; stale_rc=$?
is   "stale: exits 1 (divergence found)"       "1"              "$stale_rc"
want "stale: EXEC-TREE-STALE finding present"  "EXEC-TREE-STALE" "$stale_out"
want "stale: exec tree path named"             "$EXEC_TREE"      "$stale_out"
want "stale: missing file named"               "landed.sh"       "$stale_out"

# ===========================================================================
echo
echo "silence when exec tree is current:"
# ===========================================================================
# Copy the landed file into the exec tree so its content matches origin/main.
# git hash-object is content-addressed, so the file content must be byte-identical.
printf '#!/usr/bin/env bash\n# landed fix — absent from the exec tree\n' \
    > "$EXEC_TREE/spira/landed.sh"

current_out="$(run_skew)"; current_rc=$?
is     "current: exits 0 (no divergence)"        "0"              "$current_rc"
nowant "current: no EXEC-TREE-STALE"             "EXEC-TREE-STALE" "$current_out"

# ===========================================================================
echo
echo "no unit files — no exec tree finding:"
# ===========================================================================
# When SPIRA_UNIT_DIR is empty, exec_trees() finds no trees and the check is quiet.
EMPTY_UNIT_DIR="$TMP/empty-units"
mkdir -p "$EMPTY_UNIT_DIR"
no_unit_out="$(run_skew SPIRA_UNIT_DIR="$EMPTY_UNIT_DIR")"; no_unit_rc=$?
is     "no-units: exits 0 (nothing wrong)"       "0"              "$no_unit_rc"
nowant "no-units: no EXEC-TREE-STALE"            "EXEC-TREE-STALE" "$no_unit_out"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
