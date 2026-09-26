#!/usr/bin/env bash
#
# test-overrides.sh — operator overrides are a harness concept: `skew.sh refresh` re-applies
# them the instant it resets a checkout, and a declared override retires once HEAD carries a
# `spira: land <bead>` commit.
#
# covers: spira/overrides.sh spira/skew.sh spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-overrides.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ── Fixture: a git remote, a checkout one commit behind, and a declared override spec ────
ORIGIN="$TMP/origin"
REPO="$TMP/repo"
OVDIR="$TMP/overrides"
BEAD="sp-testov1"

git init -q "$ORIGIN"
git -C "$ORIGIN" config user.email "test@test"
git -C "$ORIGIN" config user.name "test"
mkdir -p "$ORIGIN/spira"
printf 'stock brief\n' > "$ORIGIN/brief.txt"
printf '#!/usr/bin/env bash\n# OLD\n' > "$ORIGIN/spira/lib.sh"
git -C "$ORIGIN" add spira/ brief.txt
git -C "$ORIGIN" commit -q -m "base"
BASE_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

git clone -q "$ORIGIN" "$REPO"
git -C "$REPO" config user.email "test@test"
git -C "$REPO" config user.name "test"
git -C "$REPO" remote set-head origin --auto >/dev/null 2>&1 || true

mkdir -p "$OVDIR"
cat > "$OVDIR/test-cap.override" <<EOF
BEAD="$BEAD"
needed() { [ "\$(cat "\$1/brief.txt" 2>/dev/null)" != "operator brief" ]; }
apply()  { printf '%s\n' "operator brief" > "\$1/brief.txt"; }
on_landed() { : > "\$1/.landed-marker"; }
EOF

run_skew_cmd() {
    local run_dir="$1"; shift
    env -i PATH="$PATH" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$run_dir" \
        SPIRA_OVERRIDES="$OVDIR" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        bash "$HERE/skew.sh" "$@" 2>&1
    return "${PIPESTATUS[0]:-$?}"
}

# ===========================================================================
echo
echo "refresh resets a dirty operator edit, then re-applies the declared override:"
# ===========================================================================
# The operator holds "operator brief" in the checkout ahead of the bead landing. Main moves
# (a fresh, unrelated commit); a refresh must not leave the reset brief live even briefly —
# `apply` runs synchronously inside `refresh`, so the assertion right after it returns is the
# whole test of the "no one-minute window" property.

printf 'operator brief\n' > "$REPO/brief.txt"        # operator's dirty, tracked edit
printf '# advance\n' >> "$ORIGIN/spira/lib.sh"
git -C "$ORIGIN" commit -qam "advance"

RUN1="$(mktemp -d "$TMP/run-XXXXX")"
out1="$(run_skew_cmd "$RUN1" refresh "$REPO")"; rc1=$?
is   "refresh: exits 0"                      "0"              "$rc1"
want "refresh: stashed the dirty brief"      "stashed"        "$out1"
want "refresh: reports the override applied" "test-cap: applied" "$out1"
is   "refresh: override brief present after reset" \
     "operator brief" "$(cat "$REPO/brief.txt")"

list1="$(env -i PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN1" SPIRA_OVERRIDES="$OVDIR" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    bash "$HERE/overrides.sh" list)"
want "list: override is active" "test-cap $BEAD active" "$list1"

# ===========================================================================
echo
echo "refresh retires the override once HEAD carries the land commit:"
# ===========================================================================
# The permanent fix lands for real: main's own brief.txt now carries the operator's edit, in
# a commit titled the way the sentinel names a landing. Once refresh advances the checkout
# there, on_landed must have run once and the spec must no longer be active.

printf 'operator brief\n' > "$ORIGIN/brief.txt"
git -C "$ORIGIN" commit -qam "spira: land $BEAD"
LAND_COMMIT="$(git -C "$ORIGIN" rev-parse HEAD)"

RUN2="$(mktemp -d "$TMP/run-XXXXX")"
out2="$(run_skew_cmd "$RUN2" refresh "$REPO")"; rc2=$?
is   "refresh: exits 0"                     "0"                        "$rc2"
want "refresh: reports the override retired" "test-cap: retired"       "$out2"
is   "refresh: checkout advanced to the land commit" "$LAND_COMMIT" "$(git -C "$REPO" rev-parse HEAD)"
is   "refresh: brief now carries the permanent fix, not the override's hand" \
     "operator brief" "$(cat "$REPO/brief.txt")"
[ -f "$REPO/.landed-marker" ] \
    && ok  "on_landed ran once" \
    || bad "on_landed ran once" "no .landed-marker in $REPO"
[ ! -e "$OVDIR/test-cap.override" ] \
    && ok  "spec is gone from the active directory" \
    || bad "spec is gone from the active directory" "still at $OVDIR/test-cap.override"
[ -f "$OVDIR/retired/test-cap.override" ] \
    && ok  "spec moved to retired/" \
    || bad "spec moved to retired/" "not found at $OVDIR/retired/test-cap.override"

list2="$(env -i PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN2" SPIRA_OVERRIDES="$OVDIR" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    bash "$HERE/overrides.sh" list)"
want "list: override reports retired, not active" "test-cap $BEAD retired" "$list2"
nowant "list: no longer reports active" " active" "$list2"

# A further refresh with nothing new to land must not re-run on_landed or resurrect the spec.
rm -f "$REPO/.landed-marker"
RUN3="$(mktemp -d "$TMP/run-XXXXX")"
out3="$(run_skew_cmd "$RUN3" refresh "$REPO")"; rc3=$?
is "refresh with nothing behind: exits 0" "0" "$rc3"
[ ! -f "$REPO/.landed-marker" ] \
    && ok  "retired override is never re-applied" \
    || bad "retired override is never re-applied" "on_landed ran again"

# ===========================================================================
echo
echo "SEEN RED: an override whose apply() fails is reported by doctor, not swallowed:"
# ===========================================================================
# A control that never reports a failure is indistinguishable from one that cannot detect it
# (law-absence-needs-a-positive-control). Plant a spec whose apply() always fails.

FAILDIR="$TMP/overrides-fail"
mkdir -p "$FAILDIR"
cat > "$FAILDIR/broken.override" <<'EOF'
BEAD="sp-testbroken"
needed() { return 0; }
apply()  { return 1; }
EOF

RUN4="$(mktemp -d "$TMP/run-XXXXX")"
apply_out="$(env -i PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN4" SPIRA_OVERRIDES="$FAILDIR" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    bash "$HERE/overrides.sh" apply "$REPO" 2>&1)"; apply_rc=$?
is   "apply: a failing override exits non-zero"  "1"              "$apply_rc"
want "apply: names the failing override"         "broken"         "$apply_out"

doctor_out="$(env -i PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN4" SPIRA_OVERRIDES="$FAILDIR" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    bash "$HERE/overrides.sh" doctor "$REPO")"; doctor_rc=$?
is   "doctor: a failed override is not a silent pass" "1"        "$doctor_rc"
want "doctor: names the failed override"              "broken"   "$doctor_out"

# SEEN GREEN — fix it, re-apply, and require the failed state to clear. `list`, not `doctor`,
# is the assertion here: doctor's closed-bead check calls out to `bd`, and this suite never
# touches a database (a bead id that resolves nowhere must not decide this check's outcome).
cat > "$FAILDIR/broken.override" <<'EOF'
BEAD="sp-testbroken"
needed() { return 0; }
apply()  { return 0; }
EOF
env -i PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN4" SPIRA_OVERRIDES="$FAILDIR" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    bash "$HERE/overrides.sh" apply "$REPO" >/dev/null
list4="$(env -i PATH="$PATH" SPIRA_CONF=/nonexistent SPIRA_HOME="$HERE" SPIRA_REPO="$REPO" \
    SPIRA_RUN="$RUN4" SPIRA_OVERRIDES="$FAILDIR" SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    bash "$HERE/overrides.sh" list)"
want "list: no longer failed once apply succeeds" "broken sp-testbroken active" "$list4"

echo
echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
