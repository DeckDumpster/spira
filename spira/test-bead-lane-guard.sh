#!/usr/bin/env bash
#
# test-bead-lane-guard.sh — bead.sh refuses to file a bead when the target repository
#   does not admit the persona's partition lane, and skips the check when
#   SPIRA_BEAD_LANE_OVERRIDE is set.
#
# WHAT THIS SUITE IS GUARDING
# ---------------------------
# law-a-lane-runs-only-where-the-repo-admits-it: a persona may cut work only against a
# repository whose effective lanes admit it. The first enforcement point is bead.sh file —
# the filing tool rejects before any bead enters the graph.
#
# Properties guarded:
#   1. Refusing bead names the repo, lane, and refusing declaration.
#   2. Refusal exits non-zero (no bead filed via bd create).
#   3. Same call succeeds against a repository that admits the lane.
#   4. Override SPIRA_BEAD_LANE_OVERRIDE=1 bypasses the check.
#   5. Positive control: builder (plan lane) is always admitted even against a
#      develop-only repo.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
# ---------------------------------------------------------
# Each refusal case is preceded by a success case on the same path so a broken check
# that always passes or always refuses is distinguishable.
#
# covers: spira/bead.sh spira/lib.sh
# defect: sp-4k8bf
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"
mkdir -p "$T/chamber" "$T/run"

# ---------------------------------------------------------------------------
# FAKE FAYTHS. maechen (maechen-sweep lane) and builder (plan lane).
# ---------------------------------------------------------------------------
cat > "$T/chamber/maechen.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_MAECHEN_LABEL:-maechen-sweep}"
FAYTH_TOOLS="Bash"
FAYTH_MODEL=claude-haiku-4-5-20251001
FAYTH_MAX_CONCURRENT=1
FAYTH
cat > "$T/chamber/builder.fayth" <<'FAYTH'
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_TOOLS="Bash,Edit,Write"
FAYTH_MODEL=claude-sonnet-5
FAYTH_MAX_CONCURRENT=4
FAYTH

# ---------------------------------------------------------------------------
# REPO MAP. dev-repo is develop (no maechen-sweep); self-repo is self (all lanes).
# ---------------------------------------------------------------------------
cat > "$T/repo-map" <<'MAP'
dev-repo  | /tmp/dev  | push | origin/main | | true | develop
self-repo | /tmp/self | push | origin/main | | true | self
MAP

# ---------------------------------------------------------------------------
# STUB BD. Records argv for create calls; exits 0 for all.
# ---------------------------------------------------------------------------
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    create) printf '%s\n' "$*" >> "$BD_LOG_PATH"; printf 'sp-test\n'; exit 0 ;;
    *)      exit 0 ;;
esac
STUB
chmod +x "$STUB_BD"

# Run bead.sh file in a clean environment.
run_bead() {
    : > "$BD_LOG"
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        SPIRA_DB="$T/db" \
        SPIRA_HOME="$T" \
        SPIRA_REPO="$T" \
        SPIRA_REPO_MAP="$T/repo-map" \
        SPIRA_MAECHEN_LABEL="maechen-sweep" \
        SPIRA_GROOMER_LABEL="groom" \
        SPIRA_SPIKE_LABEL="spike" \
        SPIRA_CZAR_LABEL="czar-trigger" \
        SPIRA_INCIDENT_LABEL="incident" \
        SPIRA_PLAN_LABEL="plan" \
        SPIRA_SCOPE_LABEL="spira" \
        SPIRA_BEAD_LANE_OVERRIDE="${SPIRA_BEAD_LANE_OVERRIDE:-}" \
        bash "$HERE/bead.sh" file "$@" 2>&1
}

echo
echo "test-bead-lane-guard.sh"

# ==========================================================================================
echo
echo "POSITIVE CONTROL: maechen admitted against self-repo"
# ==========================================================================================
: > "$BD_LOG"
out="$(run_bead "maechen sweep pass" --for maechen --repo self-repo)"; rc=$?
is   "self-repo: exits 0"          "0"      "$rc"
want "self-repo: bd create called" "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "REFUSE: maechen against develop-only repo"
# ==========================================================================================
# POSITIVE CONTROL: proved above that the check can pass. Now prove it refuses.
: > "$BD_LOG"
out="$(run_bead "maechen sweep pass" --for maechen --repo dev-repo)"; rc=$?
is   "dev-repo: exits non-zero"             "2"               "$rc"
want "dev-repo: names repo in message"      "dev-repo"        "$out"
want "dev-repo: names lane in message"      "maechen-sweep"   "$out"
want "dev-repo: names refusing declaration" "repo-map"        "$out"
want "dev-repo: names override variable"    "SPIRA_BEAD_LANE_OVERRIDE" "$out"
nowant "dev-repo: bd create NOT called"     "create"          "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "OVERRIDE: SPIRA_BEAD_LANE_OVERRIDE=1 bypasses the lane check"
# ==========================================================================================
: > "$BD_LOG"
out="$(SPIRA_BEAD_LANE_OVERRIDE=1 run_bead "maechen sweep pass" --for maechen --repo dev-repo)"; rc=$?
is   "override: exits 0"           "0"      "$rc"
want "override: bd create called"  "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "BUILDER (plan lane): always admitted, even against develop-only repo"
# ==========================================================================================
# plan is in every lane set; builder should never be refused regardless of repo mode.
: > "$BD_LOG"
out="$(run_bead "implement feature X" --for builder --repo dev-repo)"; rc=$?
is   "builder plan: exits 0"          "0"      "$rc"
want "builder plan: bd create called" "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "SUMMARY"
# ==========================================================================================
printf '\n  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
